use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tokio_util::codec::Framed;
use tokio_util::codec::LinesCodec;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use futures::SinkExt;

type RX = UnboundedReceiver<Vec<u8>>;
type TX = UnboundedSender<Vec<u8>>;

#[derive(Debug)]
struct Shared(HashMap<SocketAddr, TX>);

impl Shared {
    fn new() -> Self {
        Shared(HashMap::new())
    }

    async fn broadcast(&mut self, source: SocketAddr, msg: Vec<u8>) {
        for p in self.0.iter_mut() {
            if *p.0 != source {
                if let Err(err) = p.1.send(msg.clone()) {
                    eprintln!(
                        "failed to send message to peer: {:?}, reason: {:?}",
                        p.0, err
                    );
                }
            }
        }
    }

    fn remove(&mut self, source: SocketAddr) {
        self.0.remove(&source);
    }
}

#[derive(Debug)]
struct ChatService(Arc<Mutex<Shared>>);

impl ChatService {
    fn new() -> Self {
        ChatService(Arc::new(Mutex::new(Shared::new())))
    }

    async fn listen(&mut self) -> ! {
        let listener = TcpListener::bind("127.0.0.1:15555")
            .await
            .expect("failed to bind tcp listener to address 127.0.0.1:15555");

        loop {
            match listener.accept().await {
                Ok((peer_sckt, peer_addr)) => {
                    let shared = Arc::clone(&self.0);

                    tokio::spawn(async move {
                        Peer::new(peer_sckt, peer_addr, shared).await.unwrap().handle().await;
                    });
                }
                Err(err) => println!(
                    "failed to establish connection with peer, reason: {:?}",
                    err
                ),
            }
        }
    }
}

#[derive(Debug)]
struct Peer {
    sckt: Framed<TcpStream, LinesCodec>,
    rx: RX,
    shared: Arc<Mutex<Shared>>,
    username: String,
}

impl Peer {
    async fn new(sckt: TcpStream, sckt_addr: SocketAddr, state: Arc<Mutex<Shared>>) -> tokio::io::Result<Self> {
        let mut sckt = Framed::new(sckt, LinesCodec::new());

        let username = match sckt.next().await {
            Some(Ok(msg)) => {
                let mut state = state.lock().await;
                state.broadcast(sckt_addr, format!("{} has joined the room", msg).into()).await;
                msg
            },
            Some(Err(_)) => return Err(tokio::io::ErrorKind::Other.into()),
            None => return Err(tokio::io::ErrorKind::ConnectionAborted.into()),
        };

        let (tx, rx) = mpsc::unbounded_channel();

        {
            let mut peers = state.lock().await;
            peers.0.insert(sckt_addr, tx);
        }

        Ok(Peer {
            rx,
            sckt,
            username,
            shared: Arc::clone(&state),
        })
    }

    async fn handle(&mut self) {
        loop {
            tokio::select! {
                Some(msg) = self.rx.recv() => {
                    if let Err(err) = self.sckt.send(String::from_utf8_lossy(&msg)).await {
                        eprintln!("failed to write to peer: {:?}, reason: {:?}", self, err);
                    }
                },
                result = self.sckt.next() => match result {
                    Some(Ok(msg)) => {
                        let mut out = self.username.to_owned();
                        out.push_str(": ");
                        out.push_str(&String::from_utf8_lossy(msg.as_bytes()));

                        let mut shared = self.shared.lock().await;
                        let addr = self.sckt.get_ref().peer_addr().unwrap();
                        shared.broadcast(addr, out.into_bytes()).await;
                    }
                    Some(Err(err)) => {
                        eprintln!("failed to read from peer: {:?}, reason: {:?}", self, err);
                    }
                    None => break,
                },
            };
        }

        {
            let mut shared = self.shared.lock().await;
            let addr = self.sckt.get_ref().peer_addr().unwrap();
            shared.broadcast(addr, format!("{} left the room", self.username).into()).await;
            shared.remove(addr);
        }
    }
}

#[tokio::main]
async fn main() {
    ChatService::new().listen().await;
}
