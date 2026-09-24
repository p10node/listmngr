use hickory_resolver::proto::{
    op::{Message, ResponseCode},
    rr::{
        Name, RData, Record, RecordType,
        rdata::{A, MX},
    },
};
use std::net::Ipv4Addr;
use tokio::net::UdpSocket;

pub async fn serve(socket: UdpSocket) {
    let mut buffer = [0; 4096];
    loop {
        let (size, peer) = socket.recv_from(&mut buffer).await.unwrap();
        let request = Message::from_vec(&buffer[..size]).unwrap();
        let query = request.queries[0].clone();
        let name = query.name().to_string();
        let mut response = Message::response(request.id, request.op_code);
        response.metadata.recursion_desired = true;
        response.metadata.recursion_available = true;
        response.metadata.authoritative = true;
        response.add_query(query.clone());
        if name == "missing.test." {
            response.metadata.response_code = ResponseCode::NXDomain;
        } else {
            let data = match (name.as_str(), query.query_type()) {
                ("healthy.test.", RecordType::MX) => Some(RData::MX(MX::new(
                    10,
                    Name::from_ascii("mx.healthy.test.").unwrap(),
                ))),
                ("null.test.", RecordType::MX) => Some(RData::MX(MX::new(0, Name::root()))),
                ("mx.healthy.test." | "implicit.test.", RecordType::A) => {
                    Some(RData::A(A(Ipv4Addr::LOCALHOST)))
                }
                _ => None,
            };
            if let Some(data) = data {
                response.add_answer(Record::from_rdata(query.name().clone(), 60, data));
            }
        }
        socket
            .send_to(&response.to_vec().unwrap(), peer)
            .await
            .unwrap();
    }
}
