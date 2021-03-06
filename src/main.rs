extern crate xml;

mod errors;
use errors::AppError;

mod config;
use config::AppConfig;

use actix_web::{web, App, Error, HttpResponse, HttpServer};
use async_std::io;
use async_std::net::TcpStream;
use async_std::prelude::*;
use dhi_xml::{DHIRequest, DHIResponse};
use extfg_sigma::{SigmaRequest, SigmaResponse};
use futures::StreamExt;
use serde_json::Value;
use serde_xml_rs::from_reader;
use sp_xml::{SPRequest, SPResponse};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "lakgves",
    about = "Application for testing Bank credit card processing systems."
)]
struct Opt {
    /// Configuration file
    #[structopt(parse(from_os_str))]
    config: PathBuf,
}

struct AppState {
    streams: Vec<TcpStream>,
    n_connections: usize,
    conn_index: AtomicUsize,
}

impl AppState {
    pub async fn new(dhi_host: &str, n_connections: i64) -> Self {
        let mut streams: Vec<TcpStream> = Vec::new();
        let n_connections = n_connections as usize;
        for x in 0..n_connections {
            let s = TcpStream::connect(dhi_host).await.unwrap();
            s.set_nodelay(true).unwrap();
            streams.push(s);
            println!("Connection #{} established", x);
        }

        println!("Connected to {:?}", dhi_host);

        AppState {
            streams,
            n_connections,
            conn_index: AtomicUsize::new(0),
        }
    }

    fn get_stream_index(&self) -> usize {
        let indx = self.conn_index.fetch_add(1, Ordering::SeqCst);
        indx % self.n_connections
    }

    pub fn get_stream(&self) -> &TcpStream {
        let indx = self.get_stream_index();
        &self.streams[indx]
    }
}

/// Asynchronously exchange data with DHI host
async fn talk_to_dhi_host(data: web::Data<AppState>, msg: String) -> Result<DHIResponse, AppError> {
    let mut s = data.get_stream();
    let mut buffer = [0; 8192];

    // TODO: timeout from app state
    io::timeout(Duration::from_secs(5), async {
        s.write_all(&msg.as_bytes()).await?;
        s.read(&mut buffer).await?;
        Ok(())
    })
    .await?;

    // The first 5 bytes are the message length
    let response: DHIResponse = from_reader(&buffer[5..])?;
    Ok(response)
}

/// Asynchronously exchange data with SP host
async fn talk_to_sp_host(data: web::Data<AppState>, msg: String) -> Result<SPResponse, AppError> {
    let mut s = data.get_stream();
    let mut buffer = [0; 8192];

    // TODO: timeout from app state
    io::timeout(Duration::from_secs(5), async {
        s.write_all(&msg.as_bytes()).await?;
        s.read(&mut buffer).await?;
        Ok(())
    })
    .await?;

    let response = SPResponse::new(&buffer);
    Ok(response)
}

/// Asynchronously exchange data with IPS host
async fn talk_to_ips_host(
    data: web::Data<AppState>,
    msg: bytes::BytesMut,
) -> Result<SigmaResponse, AppError> {
    let mut s = data.get_stream();
    let mut buffer = [0; 8192];

    // TODO: timeout from app state
    io::timeout(Duration::from_secs(5), async {
        s.write_all(&msg).await?;
        s.read(&mut buffer).await?;
        Ok(())
    })
    .await?;

    let response = SigmaResponse::new(&buffer);
    Ok(response)
}

// TODO: write tests to cover all the unwrapping
async fn serve_dhi_request(
    data: web::Data<AppState>,
    mut body: web::Payload,
) -> Result<HttpResponse, Error> {
    let body = body.next().await.unwrap()?;
    let iso_data = String::from_utf8(body.to_vec()).unwrap();
    let iso_obj: Value = serde_json::from_str(&iso_data).unwrap();

    let r: DHIRequest = DHIRequest::new(iso_obj);
    let msg = r.serialize().unwrap();

    println!("{:?}", msg);

    let res = talk_to_dhi_host(data, msg).await;
    match res {
        Ok(res) => {
            println!("{:?}", res);
            Ok(HttpResponse::Ok()
                .content_type("application/json")
                .header("X-Hdr", "sample")
                .body(res.serialize().unwrap()))
        }
        Err(err) => match err {
            AppError::IoError(err) => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::GatewayTimeout()
                    .content_type("plain/text")
                    .body("Error communicating with DHI host"))
            }
            AppError::ParseError(err) => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::InternalServerError()
                    .content_type("plain/text")
                    .body("Error processing data from DHI host"))
            }
            AppError::SerializeError(err) => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::InternalServerError()
                    .content_type("plain/text")
                    .body("Serialization error"))
            }
            _ => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::InternalServerError()
                    .content_type("plain/text")
                    .body("Internal error"))
            }
        },
    }
}

// TODO: write tests to cover all the unwrapping
async fn serve_sp_request(
    data: web::Data<AppState>,
    mut body: web::Payload,
) -> Result<HttpResponse, Error> {
    let body = body.next().await.unwrap()?;
    let mut req = SPRequest::new(&body);

    // We've got a deserialized request, and can apply some logic.
    // The logic - generating and assinging Message ID
    req.gen_message_id();

    println!("{:?}", req);

    let msg = req.serialize().unwrap();

    // Sending stuff
    let res = talk_to_sp_host(data, msg).await;

    // Checking result of talking to host
    match res {
        Ok(res) => {
            let serialized = res.serialize().unwrap();
            println!("{:?}", serialized);
            Ok(HttpResponse::Ok()
                .content_type("text/xml")
                .header("X-Hdr", "sample")
                .body(serialized))
        }
        Err(err) => match err {
            AppError::IoError(err) => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::GatewayTimeout()
                    .content_type("plain/text")
                    .body("Error communicating with SP host"))
            }
            AppError::ParseError(err) => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::InternalServerError()
                    .content_type("plain/text")
                    .body("Error processing data from SP host"))
            }
            AppError::SerializeError(err) => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::InternalServerError()
                    .content_type("plain/text")
                    .body("Serialization error"))
            }
            _ => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::InternalServerError()
                    .content_type("plain/text")
                    .body("Internal error"))
            }
        },
    }
}

// TODO: write tests to cover all the unwrapping
async fn serve_ips_request(
    data: web::Data<AppState>,
    mut body: web::Payload,
) -> Result<HttpResponse, Error> {
    let body = body.next().await.unwrap()?;
    let payload = String::from_utf8(body.to_vec()).unwrap();
    let obj: Value = serde_json::from_str(&payload).unwrap();
    let req = SigmaRequest::new(obj).unwrap();

    println!("{:?}", req);

    let msg = req.serialize().unwrap();

    // Sending stuff
    let res = talk_to_ips_host(data, msg).await;

    // Checking result of talking to host
    match res {
        Ok(res) => {
            let serialized = res.serialize().unwrap();
            println!("{:?}", serialized);
            Ok(HttpResponse::Ok()
                .content_type("text/xml")
                .header("X-Hdr", "sample")
                .body(serialized))
        }
        Err(err) => match err {
            AppError::IoError(err) => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::GatewayTimeout()
                    .content_type("plain/text")
                    .body("Error communicating with IPS host"))
            }
            AppError::ParseError(err) => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::InternalServerError()
                    .content_type("plain/text")
                    .body("Error processing data from IPS host"))
            }
            AppError::SerializeError(err) => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::InternalServerError()
                    .content_type("plain/text")
                    .body("Serialization error"))
            }
            _ => {
                println!("Error: {:?}", err);
                Ok(HttpResponse::InternalServerError()
                    .content_type("plain/text")
                    .body("Internal error"))
            }
        },
    }
}

#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    let opt = Opt::from_args();
    let cfg = AppConfig::new(opt.config.to_str().unwrap()).unwrap();

    // TODO: iterate through channels
    let dhi_host = &cfg.channels["dhi"]["host"].as_str().unwrap();
    let n_connections = cfg.channels["dhi"]["n_connections"].as_integer().unwrap();
    let app_state = web::Data::new(AppState::new(dhi_host, n_connections).await);

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .route("/dhi", web::post().to(serve_dhi_request))
            .route("/sp", web::post().to(serve_sp_request))
            .route("/ips", web::post().to(serve_ips_request))
    })
    .workers(cfg.get_num_of_workers())
    .keep_alive(cfg.get_listener_keep_alive())
    .bind(cfg.get_conn_str())?
    .run()
    .await
}
