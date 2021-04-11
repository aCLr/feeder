use actix_web::dev::*;
use actix_web::http::header::Header;

use actix_web::{
    dev::Payload, error::ErrorUnauthorized, Error, FromRequest, HttpMessage, HttpRequest,
};
use futures::{future, task, FutureExt};
use pbkdf2::{pbkdf2_check, pbkdf2_simple};
use std::rc::Rc;

use actix_web::dev::ServiceRequest;

use crate::db::models::User;
use crate::db::queries::users as users_queries;
use crate::db::Pool;
use crate::result::Result;
use actix_web::web::Data;
use actix_web_httpauth::headers::authorization;
use futures::future::{err, ok, LocalBoxFuture};
use futures::task::Poll;
use std::cell::RefCell;

impl FromRequest for User {
    type Error = Error;
    type Future = future::Ready<Result<User, self::Error>>;
    type Config = ();

    fn from_request(req: &HttpRequest, _pl: &mut Payload) -> Self::Future {
        match req.extensions().get::<User>() {
            None => err(ErrorUnauthorized("unauthorized".to_string())),
            Some(user) => ok(user.clone()),
        }
    }
}

// It makes Middleware. It's Intermediate Object.
#[derive(Default)]
pub struct Authorization;

impl<S, R> Transform<S, R> for Authorization
where
    S: Service<R> + 'static,
    S::Future: 'static,
{
    type Response = ServiceResponse;
    type Error = Error;
    type Transform = AuthMiddleware<S>;
    type InitError = ();
    type Future = future::Ready<Result<Self::Transform, Self::InitError>>;

    // New Middlware Instance
    fn new_transform(&self, service: S) -> Self::Future {
        ok(AuthMiddleware(Rc::new(RefCell::new(service))))
    }
}

/// The actual Flash middleware
pub struct AuthMiddleware<S>(Rc<RefCell<S>>);

impl<S, R> Service<R, Error> for AuthMiddleware<S>
where
    S: Service<R> + 'static,
    S::Future: 'static,
{
    type Response = ServiceResponse;
    type Error = S::Error;
    type Future = LocalBoxFuture<'static, Result<ServiceResponse, Error>>;

    fn poll_ready(&mut self, ctx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(ctx)
    }

    fn call(&mut self, req: R) -> Self::Future {
        let service = Rc::clone(&self.0);
        let db_pool = req.app_data::<Data<Pool>>().unwrap().clone();
        let token = match authorization::Authorization::<authorization::Bearer>::parse(&req) {
            Ok(bearer) => bearer.into_scheme().token().to_string(),
            Err(err) => return Box::pin(async { Err(ErrorUnauthorized(err)) }),
        };

        async move {
            match users_queries::get_user_by_token(&db_pool, token).await? {
                None => Err(ErrorUnauthorized("unauthorized")),
                Some(user) => {
                    req.extensions_mut().insert(user);
                    service.borrow_mut().call(req).await
                }
            }
        }
        .boxed_local()
    }
}

pub fn hash(password: &str) -> String {
    pbkdf2_simple(password, 5000).unwrap()
}

pub async fn login_user(db_pool: &Pool, login: String, password: String) -> Result<User> {
    let user = users_queries::get_user_by_login(db_pool, login).await?;
    match pbkdf2_check(user.password(), password.as_str()) {
        Ok(_) => Ok(user),
        Err(_) => Err(crate::result::Error::Unauthorized(
            "invalid password".to_string(),
        )),
    }
}
