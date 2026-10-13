use super::*;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

async fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (peer, gateway) = tokio::join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept()
    );
    (peer.unwrap(), gateway.unwrap().0)
}

#[tokio::test]
async fn fin_in_either_direction_preserves_the_reverse_half() {
    for closes in [Side::Downstream, Side::Upstream] {
        let (mut guest, gateway_guest) = pair().await;
        let (mut origin, gateway_origin) = pair().await;
        let flow = Flow::default();
        let witness = flow.clone();
        let relay =
            tokio::spawn(async move { transfer(gateway_guest, gateway_origin, &witness).await });
        let (first, second) = if closes == Side::Downstream {
            (&mut guest, &mut origin)
        } else {
            (&mut origin, &mut guest)
        };
        first.write_all(b"request").await.unwrap();
        first.shutdown().await.unwrap();
        let mut request = Vec::new();
        second.read_to_end(&mut request).await.unwrap();
        assert_eq!(request, b"request");
        assert!(!relay.is_finished(), "the other half must remain usable");
        second.write_all(b"reply after FIN").await.unwrap();
        second.shutdown().await.unwrap();
        let mut reply = Vec::new();
        first.read_to_end(&mut reply).await.unwrap();
        assert_eq!(reply, b"reply after FIN");
        let bytes = relay.await.unwrap().unwrap();
        assert_eq!(
            bytes,
            if closes == Side::Downstream {
                (7, 15)
            } else {
                (15, 7)
            }
        );
    }
}

#[tokio::test]
async fn reset_from_either_peer_ends_the_relay() {
    for resets in [Side::Downstream, Side::Upstream] {
        let (guest, gateway_guest) = pair().await;
        let (origin, gateway_origin) = pair().await;
        let flow = Flow::default();
        let witness = flow.clone();
        let relay =
            tokio::spawn(async move { transfer(gateway_guest, gateway_origin, &witness).await });
        let (reset, held) = if resets == Side::Downstream {
            (guest, origin)
        } else {
            (origin, guest)
        };
        reset.set_zero_linger().unwrap();
        drop(reset);
        let error = tokio::time::timeout(Duration::from_secs(3), relay)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ECONNRESET));
        drop(held);
    }
}

#[tokio::test]
async fn simultaneous_fin_drains_both_payloads() {
    let (mut guest, gateway_guest) = pair().await;
    let (mut origin, gateway_origin) = pair().await;
    let flow = Flow::default();
    let relay = transfer(gateway_guest, gateway_origin, &flow);
    let peers = async {
        guest.write_all(b"guest").await.unwrap();
        origin.write_all(b"origin").await.unwrap();
        let (left, right) = tokio::join!(guest.shutdown(), origin.shutdown());
        left.unwrap();
        right.unwrap();
        let mut guest_reply = Vec::new();
        let mut origin_reply = Vec::new();
        let (left, right) = tokio::join!(
            guest.read_to_end(&mut guest_reply),
            origin.read_to_end(&mut origin_reply)
        );
        left.unwrap();
        right.unwrap();
        assert_eq!(guest_reply, b"origin");
        assert_eq!(origin_reply, b"guest");
    };
    let (bytes, ()) =
        tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(relay, peers) })
            .await
            .unwrap();
    assert_eq!(bytes.unwrap(), (5, 6));
}
