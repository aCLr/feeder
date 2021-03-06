use crate::models;
use crate::result::{Error, Result};
use crate::storage::Storage;
use async_trait::async_trait;
use futures::future::join_all;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{mpsc, Mutex};

pub mod http;
pub mod tg;
pub mod vk;

#[derive(Debug)]
pub enum SourceData {
    WebFeed(http::FeedUpdate),
    Telegram(tg::TelegramUpdate),
    Vk(vk::VkUpdate),
}

#[derive(Debug, PartialEq)]
pub enum Source {
    Web,
    Telegram,
    Vk,
}

#[async_trait]
pub trait UpdatesHandler<T> {
    // the place where sources created from update and stored into database
    async fn create_source(&self, updates: &T) -> Result<models::Source>;
    // the place where all updates can be stored into database
    async fn process_updates(&self, updates: &T) -> Result<usize>;
}

#[async_trait]
pub trait SourceProvider {
    // returns source name of the provider
    fn get_source(&self) -> Source;
    // TODO: receive join handle from here
    // starts provider internal routines
    async fn run(&self, updates_sender: Arc<Mutex<mpsc::Sender<Result<SourceData>>>>)
        -> Result<()>;
    // here provider searches sources; sources can be stored here
    async fn search_source(&self, query: &str) -> Result<Vec<models::Source>>;
    // on-demand synchronization
    async fn synchronize(&self, secs_depth: i32) -> Result<()>;
}

pub struct SourcesAggregator<S>
where
    S: Storage + Send + Sync + Clone + 'static,
{
    http_source: Option<Arc<http::HttpSource<S>>>,
    tg_source: Option<Arc<tg::TelegramSource<S>>>,
    vk_source: Option<Arc<vk::VkSource<S>>>,
    updates_sender: Arc<Mutex<Sender<Result<SourceData>>>>,
    updates_receiver: Mutex<Receiver<Result<SourceData>>>,
    storage: S,
}

impl<S> SourcesAggregator<S>
where
    S: Storage + Send + Sync + Clone + 'static,
{
    pub fn builder() -> UpdatesHandlerBuilder<S> {
        UpdatesHandlerBuilder::default()
    }

    pub async fn synchronize(&self, secs_depth: i32, source: Option<Source>) -> Result<()> {
        // TODO: wait all tg files downloaded
        let source_providers = self.get_enabled_sources();
        let mut tasks = vec![];
        match source {
            Some(source) => {
                for provider in &source_providers {
                    if provider.get_source() == source {
                        debug!("going to sync {:?}", provider.get_source());
                        tasks.push(provider.synchronize(secs_depth))
                    }
                }
                if tasks.is_empty() {
                    return Err(Error::SourceKindConflict(format!(
                        "can't find source {:?} in enabled sources list",
                        source
                    )));
                }
            }
            None => {
                for provider in &source_providers {
                    debug!("going to sync {:?}", provider.get_source());
                    tasks.push(provider.synchronize(secs_depth))
                }
            }
        }
        debug!("wait {} sources to sync", tasks.len());
        let tasks_results = join_all(tasks).await;
        for task_result in tasks_results {
            if let Err(err) = task_result {
                return Err(err);
            }
        }
        Ok(())
    }

    pub async fn search_source(&self, query: &str) -> Result<Vec<models::Source>> {
        let source_providers = self.get_enabled_sources();
        let mut tasks = vec![];
        for provider in &source_providers {
            tasks.push(async move {
                let found = provider.search_source(query).await;
                debug!("{:?} found {:?}", provider.get_source(), found);
                found
            })
        }
        let mut results = vec![];
        let tasks_results = join_all(tasks).await;
        for task_result in tasks_results {
            match task_result {
                Ok(res) => results.extend(res),
                Err(err) => return Err(err),
            }
        }
        results.extend(self.storage.search_source(query).await?);
        // TODO: check: duplicates appears
        results.dedup_by_key(|s| s.id);
        Ok(results)
    }

    fn get_enabled_sources(&self) -> Vec<Box<Arc<dyn SourceProvider + Send + Sync>>> {
        let mut enabled: Vec<Box<Arc<dyn SourceProvider + Send + Sync>>> = vec![];
        macro_rules! push_if_enabled {
            ($source:expr) => {
                match &$source {
                    Some(source) => enabled.push(Box::new(source.clone())),
                    None => {}
                }
            };
        }
        push_if_enabled!(self.http_source);
        push_if_enabled!(self.tg_source);
        push_if_enabled!(self.vk_source);
        enabled
    }

    pub async fn run(&self) {
        macro_rules! run_source {
            ($source:expr) => {
                match &$source {
                    None => {}
                    Some(source) => {
                        let s = source.clone();
                        let sender = self.updates_sender.clone();
                        s.run(sender).await.unwrap();
                    }
                }
            };
        }
        run_source!(self.tg_source);
        log::debug!("tg started");
        run_source!(self.http_source);
        log::debug!("http started");
        run_source!(self.vk_source);
        log::debug!("vk started");
        self.process_updates().await;
    }

    async fn process_updates(&self) {
        loop {
            while let Some(updates) = self.updates_receiver.lock().await.recv().await {
                debug!("new updates: {:?}", updates);
                let updates_result = match &updates {
                    Ok(update) => match update {
                        SourceData::WebFeed(feed_data) => match &self.http_source {
                            None => {
                                debug!("http source disabled");
                                Ok(0)
                            }
                            Some(source) => source.process_updates(feed_data).await,
                        },
                        SourceData::Telegram(telegram_update) => match &self.tg_source {
                            None => {
                                debug!("http source disabled");
                                Ok(0)
                            }
                            Some(source) => source.process_updates(telegram_update).await,
                        },
                        SourceData::Vk(vk_update) => match &self.vk_source {
                            None => {
                                debug!("vk source disabled");
                                Ok(0)
                            }
                            Some(source) => source.process_updates(vk_update).await,
                        },
                    },
                    Err(err) => Err(Error::DbError(err.to_string())),
                };
                match updates_result {
                    Ok(ok_processed) => {
                        debug!("processed updates: {}", ok_processed);
                        trace!("updates: {:?}", updates);
                    }
                    Err(err) => {
                        error!("{}", err);
                    }
                }
            }
        }
    }
}

pub struct UpdatesHandlerBuilder<S>
where
    S: Storage + Send + Sync + Clone + 'static,
{
    http_source: Option<Arc<http::HttpSource<S>>>,
    tg_source: Option<Arc<tg::TelegramSource<S>>>,
    vk_source: Option<Arc<vk::VkSource<S>>>,
    storage: Option<S>,
}

impl<S> Default for UpdatesHandlerBuilder<S>
where
    S: Storage + Send + Sync + Clone + 'static,
{
    fn default() -> Self {
        Self {
            http_source: None,
            tg_source: None,
            vk_source: None,
            storage: None,
        }
    }
}

impl<S> UpdatesHandlerBuilder<S>
where
    S: Storage + Send + Sync + Clone + 'static,
{
    pub fn with_storage(mut self, storage: S) -> Self {
        self.storage = Some(storage);
        self
    }
    pub fn with_http_source(mut self, http_source: Arc<http::HttpSource<S>>) -> Self {
        self.http_source = Some(http_source);
        self
    }

    pub fn with_tg_source(mut self, tg_source: Arc<tg::TelegramSource<S>>) -> Self {
        self.tg_source = Some(tg_source);
        self
    }

    pub fn with_vk_source(mut self, vk_source: Arc<vk::VkSource<S>>) -> Self {
        self.vk_source = Some(vk_source);
        self
    }

    pub fn build(self) -> SourcesAggregator<S> {
        if self.storage.is_none() {
            panic!("storage not passed");
        }
        let (updates_sender, updates_receiver) = mpsc::channel::<Result<SourceData>>(2000);
        let updates_sender = Arc::new(Mutex::new(updates_sender));
        let updates_receiver = Mutex::new(updates_receiver);
        SourcesAggregator {
            http_source: self.http_source,
            tg_source: self.tg_source,
            vk_source: self.vk_source,
            storage: self.storage.unwrap(),
            updates_sender,
            updates_receiver,
        }
    }
}
