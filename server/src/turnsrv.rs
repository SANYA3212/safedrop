//! TURN для звонков и демонстраций экрана через сервер.
//!
//! Обычный TURN-сервер пересылает UDP куда угодно, и любой, у кого есть
//! пароль, мог бы слать пакеты с этого сервера в чужие сети или в его же
//! локальные службы. Здесь пересылка разрешена только между выделенными
//! этим же сервером адресами: SafeDrop в режиме сервера разрешает WebRTC
//! только relay-кандидаты, поэтому обе стороны звонка — наши адреса.
//! Пакеты между ними идут через 127.0.0.1 (не зависит от того, умеет ли
//! провайдер «петлю» на белый IP), а получателю показывается белый адрес,
//! как и положено по TURN.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use turn::auth::{generate_long_term_credentials, LongTermAuthHandler};
use turn::relay::RelayAddressGenerator;
use turn::server::config::{ConnConfig, ServerConfig};
use turn::server::Server;
use webrtc_util::Conn;

/// Срок данных для входа в TURN; клиент получает новые раньше (см. main).
pub const CRED_TTL: Duration = Duration::from_secs(24 * 3600);

pub struct Turn {
    _server: Server,
    secret: String,
    pub port: u16,
}

impl Turn {
    /// Имя и пароль TURN (по общему секрету, как в coturn `use-auth-secret`).
    pub fn credentials(&self) -> Option<(String, String, u64)> {
        let (user, cred) = generate_long_term_credentials(&self.secret, CRED_TTL).ok()?;
        let expires = user.parse::<u64>().unwrap_or(0);
        Some((user, cred, expires))
    }
}

pub async fn start(port: u16, public_ip: Ipv4Addr, min_port: u16, max_port: u16) -> Result<Turn, String> {
    if min_port == 0 || max_port < min_port {
        return Err("неверный диапазон портов TURN".into());
    }
    let secret = crate::random_hex(32);
    let sock = tokio::net::UdpSocket::bind(("0.0.0.0", port))
        .await
        .map_err(|e| format!("TURN: UDP {port}: {e}"))?;
    let server = Server::new(ServerConfig {
        conn_configs: vec![ConnConfig {
            conn: Arc::new(sock),
            relay_addr_generator: Box::new(LoopRelay { public_ip, min_port, max_port }),
        }],
        realm: "safedrop".into(),
        auth_handler: Arc::new(LongTermAuthHandler::new(secret.clone())),
        channel_bind_timeout: Duration::from_secs(0),
        alloc_close_notify: None,
    })
    .await
    .map_err(|e| format!("TURN: {e}"))?;
    Ok(Turn { _server: server, secret, port })
}

struct LoopRelay {
    public_ip: Ipv4Addr,
    min_port: u16,
    max_port: u16,
}

#[async_trait]
impl RelayAddressGenerator for LoopRelay {
    fn validate(&self) -> Result<(), turn::Error> {
        Ok(())
    }

    fn supports_family(&self, use_ipv4: bool) -> bool {
        use_ipv4
    }

    async fn allocate_conn(
        &self,
        use_ipv4: bool,
        requested_port: u16,
    ) -> Result<(Arc<dyn Conn + Send + Sync>, SocketAddr), turn::Error> {
        if !use_ipv4 {
            return Err(turn::Error::Other("только IPv4".into()));
        }
        let span = (self.max_port - self.min_port) as u32 + 1;
        let tries: Vec<u16> = if requested_port != 0 {
            if !(self.min_port..=self.max_port).contains(&requested_port) {
                return Err(turn::Error::Other("порт вне диапазона".into()));
            }
            vec![requested_port]
        } else {
            (0..span.min(40)).map(|_| self.min_port + (crate::random_u32() % span) as u16).collect()
        };
        for p in tries {
            if let Ok(sock) = tokio::net::UdpSocket::bind(("0.0.0.0", p)).await {
                let conn = LoopConn { sock, public_ip: self.public_ip, min_port: self.min_port, max_port: self.max_port };
                return Ok((Arc::new(conn), SocketAddr::new(IpAddr::V4(self.public_ip), p)));
            }
        }
        Err(turn::Error::Other("нет свободных портов TURN".into()))
    }
}

/// Сокет выделенного адреса: шлёт только на другие наши адреса (через петлю).
struct LoopConn {
    sock: tokio::net::UdpSocket,
    public_ip: Ipv4Addr,
    min_port: u16,
    max_port: u16,
}

impl LoopConn {
    fn ours(&self, port: u16) -> bool {
        (self.min_port..=self.max_port).contains(&port)
    }
}

/// Куда на самом деле отправить пакет для `target` (None — запрещено).
fn route(target: SocketAddr, public_ip: Ipv4Addr, ours: impl Fn(u16) -> bool) -> Option<SocketAddr> {
    match target.ip() {
        IpAddr::V4(ip) if (ip == public_ip || ip.is_loopback()) && ours(target.port()) => {
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), target.port()))
        }
        _ => None,
    }
}

/// Кем показать отправителя пришедшего пакета (None — выбросить).
fn source(src: SocketAddr, public_ip: Ipv4Addr, ours: impl Fn(u16) -> bool) -> Option<SocketAddr> {
    match src.ip() {
        IpAddr::V4(ip) if ip.is_loopback() && ours(src.port()) => Some(SocketAddr::new(IpAddr::V4(public_ip), src.port())),
        _ => None,
    }
}

#[async_trait]
impl Conn for LoopConn {
    async fn connect(&self, _addr: SocketAddr) -> webrtc_util::Result<()> {
        Err(webrtc_util::Error::Other("not supported".into()))
    }

    async fn recv(&self, buf: &mut [u8]) -> webrtc_util::Result<usize> {
        Ok(self.recv_from(buf).await?.0)
    }

    async fn recv_from(&self, buf: &mut [u8]) -> webrtc_util::Result<(usize, SocketAddr)> {
        loop {
            let (n, src) = self.sock.recv_from(buf).await?;
            if let Some(shown) = source(src, self.public_ip, |p| self.ours(p)) {
                return Ok((n, shown));
            }
        }
    }

    async fn send(&self, _buf: &[u8]) -> webrtc_util::Result<usize> {
        Err(webrtc_util::Error::Other("not supported".into()))
    }

    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> webrtc_util::Result<usize> {
        match route(target, self.public_ip, |p| self.ours(p)) {
            Some(to) => Ok(self.sock.send_to(buf, to).await?),
            // чужой адрес: молча выбрасываем, как это делает сеть
            None => Ok(buf.len()),
        }
    }

    fn local_addr(&self) -> webrtc_util::Result<SocketAddr> {
        Ok(self.sock.local_addr()?)
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        None
    }

    async fn close(&self) -> webrtc_util::Result<()> {
        Ok(())
    }

    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_only_between_own_addresses() {
        let pubip = Ipv4Addr::new(203, 0, 113, 7);
        let ours = |p: u16| (50000..=50010).contains(&p);
        let a = |ip: [u8; 4], p: u16| SocketAddr::from((ip, p));
        assert_eq!(route(a([203, 0, 113, 7], 50003), pubip, ours), Some(a([127, 0, 0, 1], 50003)));
        assert_eq!(route(a([203, 0, 113, 7], 22), pubip, ours), None);
        assert_eq!(route(a([127, 0, 0, 1], 53), pubip, ours), None);
        assert_eq!(route(a([8, 8, 8, 8], 50003), pubip, ours), None);
        assert_eq!(route(a([192, 168, 1, 1], 50003), pubip, ours), None);
        assert_eq!(source(a([127, 0, 0, 1], 50004), pubip, ours), Some(a([203, 0, 113, 7], 50004)));
        assert_eq!(source(a([127, 0, 0, 1], 9999), pubip, ours), None);
        assert_eq!(source(a([8, 8, 8, 8], 50004), pubip, ours), None);
    }
}
