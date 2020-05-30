use actix::io::SinkWrite;
use actix::prelude::*;
use actix_web::{middleware, web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;
use std::time::{Duration, Instant};
use futures::stream::StreamExt;

mod actors;
mod connector;
use actors::btc::BTCWebsocketActor;

/// How often heartbeat pings are sent
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// How long before lack of client response causes a timeout
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Do websocket handshake and start `WebSocketActor` actor
async fn ws_index(
    r: HttpRequest,
    stream: web::Payload,
    btc_actor_address: web::Data<actix::Addr<BTCWebsocketActor>>,
) -> Result<HttpResponse, Error> {
    println!("{:?}", r);

    let web_socket_actor = WebSocketActor::new();
    let res = ws::start(web_socket_actor, &r, stream);

    println!("{:?}", res);
    res
}

/// Websocket connection is a long running connection, it easier to handle with
/// an actor.
struct WebSocketActor {
    /// Client must send ping at least once per 10 seconds (CLIENT_TIMEOUT),
    /// otherwise we drop connection.
    hb: Instant,
}

impl Actor for WebSocketActor {
    type Context = ws::WebsocketContext<Self>;

    /// Method is called on actor start. We start the heartbeat process here.
    fn started(&mut self, ctx: &mut Self::Context) {
        self.hb(ctx);
    }
}

/// Handler for `ws::Message`
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WebSocketActor {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        // process websocket messages
        println!("WS: {:?}", msg);
        match msg {
            Ok(ws::Message::Ping(msg)) => {
                self.hb = Instant::now();
                ctx.pong(&msg);
            }
            Ok(ws::Message::Pong(_)) => {
                self.hb = Instant::now();
            }
            Ok(ws::Message::Text(text)) => ctx.text(text),
            Ok(ws::Message::Binary(bin)) => ctx.binary(bin),
            Ok(ws::Message::Close(_)) => {
                ctx.stop();
            }
            _ => ctx.stop(),
        }
    }
}

impl WebSocketActor {
    fn new() -> Self {
        Self { hb: Instant::now() }
    }

    /// helper method that sends ping to client every second.
    ///
    /// also this method checks heartbeats from client
    fn hb(&self, ctx: &mut <Self as Actor>::Context) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            // check client heartbeats
            if Instant::now().duration_since(act.hb) > CLIENT_TIMEOUT {
                // heartbeat timed out
                println!("Websocket client heartbeat failed, disconnecting!");

                // stop actor
                ctx.stop();

                // don't try to send a ping
                return;
            }

            ctx.ping(b"");
        });
    }
}

#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "actix_server=info,actix_web=info");
    env_logger::init();

    let client = connector::createClient();
    let (_response, framed) = client
        .ws("wss://ws.blockchain.info/inv")
        .connect()
        .await
        .map_err(|err| panic!("Error trying to connect: {}", err))
        .unwrap();
    let (sink, stream) = framed.split();
    let btc_actor_address = BTCWebsocketActor::create(|ctx| {
        BTCWebsocketActor::add_stream(stream, ctx);
        BTCWebsocketActor(SinkWrite::new(sink, ctx))
    });

    let recipient = btc_actor_address.recipient();

    HttpServer::new(move || {
        App::new()
            .data(btc_actor_address.clone())
            .wrap(middleware::Logger::default())
            .service(web::resource("/ws/").route(web::get().to(ws_index)))
    })
    // start http server on 127.0.0.1:8080
    .bind("127.0.0.1:8080")?
    .run()
    .await
}