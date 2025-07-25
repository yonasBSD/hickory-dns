#![allow(dead_code)]
#![allow(clippy::dbg_macro)]

use std::{
    fmt,
    future::poll_fn,
    io, mem,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use futures::{
    Future, FutureExt,
    future::{self, BoxFuture},
    stream::{Stream, StreamExt},
};
#[cfg(feature = "__dnssec")]
use tokio::net::UdpSocket;
use tokio::time::{Duration, Instant, Sleep};

#[cfg(feature = "__dnssec")]
use hickory_client::client::DnssecClient;
use hickory_proto::{
    BufDnsStreamHandle, ProtoError,
    op::Message,
    rr::Record,
    runtime::TokioTime,
    serialize::binary::{BinDecodable, BinDecoder, BinEncoder},
    xfer::{DnsClientStream, DnsResponse, Protocol, SerialMessage, StreamReceiver},
};
#[cfg(feature = "__dnssec")]
use hickory_proto::{
    dnssec::{PublicKeyBuf, SigningKey, TrustAnchors, crypto::Ed25519SigningKey},
    runtime::TokioRuntimeProvider,
    udp::UdpClientStream,
};
#[cfg(feature = "__dnssec")]
use hickory_server::Server;
use hickory_server::{
    authority::{Catalog, MessageResponse},
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
};

pub mod example_authority;
pub mod mock_client;
pub mod mock_request_handler;

pub struct TestClientStream {
    catalog: Arc<Mutex<Catalog>>,
    outbound_messages: StreamReceiver,
}

impl TestClientStream {
    pub fn new(
        catalog: Arc<Mutex<Catalog>>,
    ) -> (
        BoxFuture<'static, Result<Self, ProtoError>>,
        BufDnsStreamHandle,
    ) {
        let (message_sender, outbound_messages) = BufDnsStreamHandle::new(([0, 0, 0, 0], 0).into());

        let stream = Box::pin(future::ok(TestClientStream {
            catalog,
            outbound_messages,
        }));

        (stream, message_sender)
    }
}

#[derive(Clone, Default)]
pub struct TestResponseHandler {
    message_ready: Arc<AtomicBool>,
    buf: Arc<Mutex<Vec<u8>>>,
}

impl TestResponseHandler {
    pub fn new() -> Self {
        let buf = Arc::new(Mutex::new(Vec::with_capacity(512)));
        let message_ready = Arc::new(AtomicBool::new(false));
        TestResponseHandler { message_ready, buf }
    }

    fn into_inner(self) -> impl Future<Output = Vec<u8>> {
        poll_fn(move |_| {
            if self
                .message_ready
                .compare_exchange(true, false, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                let bytes: Vec<u8> = mem::take(&mut self.buf.lock().unwrap());
                Poll::Ready(bytes)
            } else {
                Poll::Pending
            }
        })
    }

    pub async fn into_message(self) -> Message {
        let bytes = self.into_inner().await;
        let mut decoder = BinDecoder::new(&bytes);
        Message::read(&mut decoder).expect("could not decode message")
    }
}

#[async_trait::async_trait]
impl ResponseHandler for TestResponseHandler {
    async fn send_response<'a>(
        &mut self,
        response: MessageResponse<
            '_,
            'a,
            impl Iterator<Item = &'a Record> + Send + 'a,
            impl Iterator<Item = &'a Record> + Send + 'a,
            impl Iterator<Item = &'a Record> + Send + 'a,
            impl Iterator<Item = &'a Record> + Send + 'a,
        >,
    ) -> io::Result<ResponseInfo> {
        let buf = &mut self.buf.lock().unwrap();
        buf.clear();
        let mut encoder = BinEncoder::new(buf);
        let info = response
            .destructive_emit(&mut encoder)
            .expect("could not encode");
        self.message_ready.store(true, Ordering::Release);
        Ok(info)
    }
}

impl fmt::Display for TestClientStream {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(formatter, "TestClientStream")
    }
}

impl DnsClientStream for TestClientStream {
    type Time = TokioTime;

    fn name_server_addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 1234))
    }
}

impl Stream for TestClientStream {
    type Item = Result<SerialMessage, ProtoError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        use futures::executor::block_on;

        match self.outbound_messages.next().poll_unpin(cx) {
            // already handled above, here to make sure the poll() pops the next message
            Poll::Ready(Some(message)) => {
                let (bytes, _) = message.into_parts();
                let src_addr = SocketAddr::from(([127, 0, 0, 1], 1234));
                let request = Request::from_bytes(bytes, src_addr, Protocol::Udp).unwrap();

                let response_handler = TestResponseHandler::new();
                block_on(
                    self.catalog
                        .lock()
                        .unwrap()
                        .handle_request(&request, response_handler.clone()),
                );

                let buf = block_on(response_handler.into_inner());
                Poll::Ready(Some(Ok(SerialMessage::new(buf, src_addr))))
            }
            // now we get to drop through to the receives...
            Poll::Ready(None) => Poll::Ready(None),
            // TODO: should we also return None if there are no more messages to send?
            Poll::Pending => {
                //dbg!("PENDING");
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
}

impl fmt::Debug for TestClientStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "TestClientStream catalog")
    }
}

// need to do something with the message channel, otherwise the Client will think there
//  is no one listening to messages and shutdown...
pub struct NeverReturnsClientStream {
    timeout: Pin<Box<Sleep>>,
    outbound_messages: StreamReceiver,
}

#[allow(dead_code)]
impl NeverReturnsClientStream {
    pub fn new() -> (
        BoxFuture<'static, Result<Self, ProtoError>>,
        BufDnsStreamHandle,
    ) {
        let (message_sender, outbound_messages) = BufDnsStreamHandle::new(([0, 0, 0, 0], 0).into());

        let stream = Box::pin(future::lazy(|_| {
            Ok(NeverReturnsClientStream {
                timeout: Box::pin(tokio::time::sleep(Duration::from_secs(1))),
                outbound_messages,
            })
        }));

        (stream, message_sender)
    }
}

impl fmt::Display for NeverReturnsClientStream {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(formatter, "NeverReturnsClientStream")
    }
}

impl DnsClientStream for NeverReturnsClientStream {
    type Time = TokioTime;

    fn name_server_addr(&self) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], 53))
    }
}

impl Stream for NeverReturnsClientStream {
    type Item = Result<SerialMessage, ProtoError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        // poll the timer forever...
        if self.timeout.poll_unpin(cx).is_pending() {
            return Poll::Pending;
        }

        self.timeout
            .as_mut()
            .reset(Instant::now() + Duration::from_secs(1));

        match self.timeout.poll_unpin(cx) {
            Poll::Pending => Poll::Pending,
            _ => panic!("timeout fired early"),
        }
    }
}

impl fmt::Debug for NeverReturnsClientStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "TestClientStream catalog")
    }
}

/// Generate an Ed25519 key pair.
#[cfg(feature = "__dnssec")]
pub fn generate_key() -> (Box<dyn SigningKey>, PublicKeyBuf) {
    let signing_key =
        Ed25519SigningKey::from_pkcs8(&Ed25519SigningKey::generate_pkcs8().unwrap()).unwrap();
    let public_key = signing_key.to_public_key().unwrap();
    (Box::new(signing_key), public_key)
}

/// Creates a server using a request handler, and creates a validating client connected to the
/// server.
#[cfg(feature = "__dnssec")]
pub async fn setup_dnssec_client_server<H>(
    handler: H,
    public_key: &PublicKeyBuf,
) -> (DnssecClient, Server<H>)
where
    H: RequestHandler,
{
    // Server setup
    let udp_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let local_addr = udp_socket.local_addr().unwrap();
    let mut server = Server::new(handler);
    server.register_socket(udp_socket);

    // Client setup
    let mut trust_anchor = TrustAnchors::empty();
    trust_anchor.insert(public_key);
    let stream = UdpClientStream::builder(local_addr, TokioRuntimeProvider::new()).build();
    let (client, bg) = DnssecClient::builder(stream)
        .trust_anchor(trust_anchor)
        .build()
        .await
        .unwrap();
    tokio::spawn(bg);

    (client, server)
}

/// Prints a response in textual form.
pub fn print_response(response: &DnsResponse) {
    for (section_heading, section) in [
        ("; Answers", response.answers()),
        ("; Authorities", response.authorities()),
        ("; Additionals", response.additionals()),
    ] {
        println!("{section_heading}");
        for record in section {
            println!("{record}");
        }
    }
}

pub const GOOGLE_V4: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53));
pub const GOOGLE_V6: SocketAddr = SocketAddr::V6(SocketAddrV6::new(
    Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888),
    53,
    0,
    0,
));
pub const CLOUDFLARE_V4_TLS: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 443));
pub const TEST3_V4: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 1), 53));
