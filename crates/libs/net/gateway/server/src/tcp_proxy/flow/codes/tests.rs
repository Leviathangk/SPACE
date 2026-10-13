use super::*;

#[tokio::test]
async fn refused_tcp_connection_retains_the_peer_reset_and_errno() {
    // Keep the port reserved without listening, so another test cannot
    // reuse it before connect.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let error = tokio::net::TcpStream::connect(socket.local_addr().unwrap())
        .await
        .unwrap_err();
    let codes = Codes::io(&error);
    assert_eq!(codes.errno, Some(Errno::ECONNREFUSED));
    assert_eq!(codes.error_type(), "ECONNREFUSED");
    assert_eq!(codes.close().as_str(), "remote_reset");
}
