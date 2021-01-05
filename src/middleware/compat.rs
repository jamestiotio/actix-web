//! `Middleware` for enabling any middleware to be used in `Resource`, `Scope` and `Condition`.
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use actix_http::body::Body;
use actix_http::body::{MessageBody, ResponseBody};
use actix_service::{Service, Transform};
use futures_core::future::LocalBoxFuture;
use futures_core::ready;

use crate::error::Error;
use crate::service::ServiceResponse;

/// `Middleware` for enabling any middleware to be used in `Resource`, `Scope` and `Condition`.
///
///
/// ## Usage
///
/// ```rust
/// use actix_web::middleware::{Logger, Compat};
/// use actix_web::{App, web};
///
/// let logger = Logger::default();
///
/// // this would not compile
/// // let app = App::new().service(web::scope("scoped").wrap(logger));
///
/// // by using scoped middleware we can use logger in scope.
/// let app = App::new().service(web::scope("scoped").wrap(Compat::new(logger)));
/// ```
pub struct Compat<T> {
    transform: T,
}

impl<T> Compat<T> {
    pub fn new(transform: T) -> Self {
        Self { transform }
    }
}

impl<S, T, Req> Transform<S, Req> for Compat<T>
where
    S: Service<Req>,
    T: Transform<S, Req>,
    T::Future: 'static,
    T::Response: MapServiceResponseBody,
    Error: From<T::Error>,
{
    type Response = ServiceResponse;
    type Error = Error;
    type Transform = CompatMiddleware<T::Transform>;
    type InitError = T::InitError;
    type Future = LocalBoxFuture<'static, Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        let fut = self.transform.new_transform(service);
        Box::pin(async move {
            let service = fut.await?;
            Ok(CompatMiddleware { service })
        })
    }
}

pub struct CompatMiddleware<S> {
    service: S,
}

impl<S, Req> Service<Req> for CompatMiddleware<S>
where
    S: Service<Req>,
    S::Response: MapServiceResponseBody,
    Error: From<S::Error>,
{
    type Response = ServiceResponse;
    type Error = Error;
    type Future = CompatMiddlewareFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx).map_err(From::from)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let fut = self.service.call(req);
        CompatMiddlewareFuture { fut }
    }
}

#[pin_project::pin_project]
pub struct CompatMiddlewareFuture<Fut> {
    #[pin]
    fut: Fut,
}

impl<Fut, T, E> Future for CompatMiddlewareFuture<Fut>
where
    Fut: Future<Output = Result<T, E>>,
    T: MapServiceResponseBody,
    Error: From<E>,
{
    type Output = Result<ServiceResponse, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = ready!(self.project().fut.poll(cx))?;
        Poll::Ready(Ok(res.map_body()))
    }
}

// trait for convert ServiceResponse's ResponseBody<B> generic type
// to ResponseBody<Body>
pub trait MapServiceResponseBody {
    fn map_body(self) -> ServiceResponse;
}

impl<B: MessageBody + Unpin + 'static> MapServiceResponseBody for ServiceResponse<B> {
    fn map_body(self) -> ServiceResponse {
        self.map_body(|_, body| ResponseBody::Other(Body::from_message(body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use actix_service::IntoService;

    use crate::dev::ServiceRequest;
    use crate::http::StatusCode;
    use crate::middleware::{Compress, Condition, Logger};
    use crate::test::{call_service, init_service, TestRequest};
    use crate::{web, App, HttpResponse};

    #[actix_rt::test]
    async fn test_scope_middleware() {
        let logger = Logger::default();
        let compress = Compress::default();

        let mut srv = init_service(
            App::new().service(
                web::scope("app")
                    .wrap(Compat::new(logger))
                    .wrap(Compat::new(compress))
                    .service(
                        web::resource("/test").route(web::get().to(HttpResponse::Ok)),
                    ),
            ),
        )
        .await;

        let req = TestRequest::with_uri("/app/test").to_request();
        let resp = call_service(&mut srv, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_rt::test]
    async fn test_resource_scope_middleware() {
        let logger = Logger::default();
        let compress = Compress::default();

        let mut srv = init_service(
            App::new().service(
                web::resource("app/test")
                    .wrap(Compat::new(logger))
                    .wrap(Compat::new(compress))
                    .route(web::get().to(HttpResponse::Ok)),
            ),
        )
        .await;

        let req = TestRequest::with_uri("/app/test").to_request();
        let resp = call_service(&mut srv, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_rt::test]
    async fn test_condition_scope_middleware() {
        let srv = |req: ServiceRequest| {
            Box::pin(async move {
                Ok(req.into_response(HttpResponse::InternalServerError().finish()))
            })
        };

        let logger = Logger::default();

        let mut mw = Condition::new(true, Compat::new(logger))
            .new_transform(srv.into_service())
            .await
            .unwrap();
        let resp = call_service(&mut mw, TestRequest::default().to_srv_request()).await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}