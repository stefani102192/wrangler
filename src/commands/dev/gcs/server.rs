use crate::commands::dev::gcs::headers::{destructure_response, structure_request};
use crate::commands::dev::{
    tls::{get_tls_acceptor, io_error, HyperAcceptor},
    ServerConfig,
};
use crate::terminal::emoji;

use std::sync::{Arc, Mutex};

use chrono::prelude::*;

use hyper::client::{HttpConnector, ResponseFuture};
use hyper::header::{HeaderName, HeaderValue};
use hyper::http::uri::InvalidUri;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client as HyperClient, Request, Response, Server, Uri};
use hyper_rustls::HttpsConnector;

use futures_util::{
    future::TryFutureExt,
    stream::{StreamExt, TryStreamExt},
};

use tokio::net::TcpListener;

const PREVIEW_HOST: &str = "rawhttp.cloudflareworkers.com";

/// performs all logic that takes an incoming request
/// and routes it to the Workers runtime preview service
pub(super) async fn serve(
    server_config: ServerConfig,
    preview_id: Arc<Mutex<String>>,
) -> Result<(), failure::Error> {
    // set up https client to connect to the preview service
    let https = HttpsConnector::new();
    let client = HyperClient::builder().build::<_, Body>(https);

    let listening_address = server_config.listening_address;

    // Create a TCP listener via tokio.
    let mut tcp = TcpListener::bind(&listening_address).await?;
    let tls_acceptor = get_tls_acceptor()?;
    // Prepare a long-running future stream to accept and serve cients.
    let incoming_tls_stream = tcp
        .incoming()
        .map_err(|e| io_error(format!("Incoming connection failed: {:?}", e)))
        .and_then(move |s| {
            tls_acceptor
                .accept(s)
                .map_err(|e| io_error(format!("TLS Error: {:?}", e)))
        })
        .boxed();

    // create a closure that hyper will use later to handle HTTP requests
    // this takes care of sending an incoming request along to
    // the uploaded Worker script and returning its response
    let service = make_service_fn(move |_| {
        let client = client.to_owned();
        let server_config = server_config.to_owned();
        let preview_id = preview_id.to_owned();
        async move {
            Ok::<_, failure::Error>(service_fn(move |req| {
                let client = client.to_owned();
                let server_config = server_config.to_owned();
                let preview_id = preview_id.lock().unwrap().to_owned();
                let version = req.version();

                // record the time of the request
                let now: DateTime<Local> = Local::now();

                // split the request into parts so we can read
                // what it contains and display in logs
                let (parts, body) = req.into_parts();

                let req_method = parts.method.to_string();

                // parse the path so we can send it to the preview service
                // we don't want to send "localhost:8787/path", just "/path"
                let path = get_path_as_str(&parts.uri);

                async move {
                    // send the request to the preview service
                    let resp = preview_request(
                        Request::from_parts(parts, body),
                        client,
                        preview_id.to_owned(),
                    )
                    .await?;
                    let (mut parts, body) = resp.into_parts();

                    // format the response for the user
                    destructure_response(&mut parts)?;
                    let resp = Response::from_parts(parts, body);

                    // print information about the response
                    // [2020-04-20 15:25:54] GET example.com/ HTTP/1.1 200 OK
                    println!(
                        "[{}] {} {}{} {:?} {}",
                        now.format("%Y-%m-%d %H:%M:%S"),
                        req_method,
                        server_config.host,
                        path,
                        version,
                        resp.status()
                    );
                    Ok::<_, failure::Error>(resp)
                }
            }))
        }
    });

    let server = Server::builder(HyperAcceptor {
        acceptor: incoming_tls_stream,
    })
    .serve(service);
    println!(
        "{} Listening on http://{}",
        emoji::EAR,
        listening_address.to_string()
    );
    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
    Ok(())
}

fn preview_request(
    req: Request<Body>,
    client: HyperClient<HttpsConnector<HttpConnector>>,
    preview_id: String,
) -> ResponseFuture {
    let (mut parts, body) = req.into_parts();

    let path = get_path_as_str(&parts.uri);
    let preview_id = &preview_id;

    structure_request(&mut parts);

    parts.headers.insert(
        HeaderName::from_static("host"),
        HeaderValue::from_static(PREVIEW_HOST),
    );

    parts.headers.insert(
        HeaderName::from_static("cf-ew-preview"),
        HeaderValue::from_str(preview_id).expect("Could not create header for preview id"),
    );

    parts.uri = get_preview_url(&path).expect("Could not get preview url");

    let req = Request::from_parts(parts, body);

    client.request(req)
}

fn get_preview_url(path_string: &str) -> Result<Uri, InvalidUri> {
    format!("https://{}{}", PREVIEW_HOST, path_string).parse()
}

fn get_path_as_str(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}
