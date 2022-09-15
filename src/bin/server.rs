use std::{net::UdpSocket, thread, time::Duration};

fn main() {
    let socket = UdpSocket::bind("127.0.0.1:42070").expect("couldn't bind to address");
    loop {
        socket.send_to(&[0; 0], "127.0.0.1:42069").expect("couldn't send data");
        thread::sleep(Duration::from_millis(1));
    }
}
