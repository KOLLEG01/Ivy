use ivy_contracts::{
    IvyError, IvyJson as J, JsString,
    json::{parse, stringify},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub async fn receive(socket: &mut TcpStream) -> (J, J) {
    let mut bytes = Vec::new();
    let mut part = [0; 4096];
    let (end, length) = loop {
        let count = socket.read(&mut part).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&part[..count]);
        if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            let text = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length: usize = text
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                break (end, length);
            }
        }
    };
    let header = JsString(bytes[..end].iter().map(|byte| u16::from(*byte)).collect())
        .to_utf8()
        .unwrap();
    let authorization = header.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .starts_with("authorization:")
            .then(|| {
                let value = &line[line.find(':').unwrap() + 1..];
                let value = value.strip_prefix(' ').unwrap_or(value);
                value
                    .chars()
                    .map(|character| format!("{:02x}", character as u32))
                    .collect::<String>()
            })
    });
    let frame = parse(
        std::str::from_utf8(&bytes[end + 4..end + 4 + length]).unwrap(),
        usize::MAX,
    )
    .unwrap();
    let trace = J::object([
        (
            "path",
            header
                .lines()
                .next()
                .unwrap()
                .split(' ')
                .nth(1)
                .unwrap()
                .into(),
        ),
        (
            "authorization",
            authorization.map(J::from).unwrap_or(J::Null),
        ),
        ("method", frame.get("method").unwrap().clone()),
        ("params", frame.get("params").unwrap().clone()),
    ]);
    (frame, trace)
}
pub async fn reply(socket: &mut TcpStream, request: &J, result: Result<J, IvyError>) {
    let mut fields = vec![
        ("jsonrpc", "2.0".into()),
        ("id", request.get("id").unwrap().clone()),
    ];
    match result {
        Ok(value) => fields.push(("result", value)),
        Err(error) => fields.push(("error", error.to_wire())),
    }
    let body = stringify(&J::object(fields), usize::MAX).unwrap();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
}
