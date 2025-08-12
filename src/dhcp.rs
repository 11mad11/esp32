use edge_dhcp::{server::Action, DhcpOption, Ipv4Addr, MessageType, Options, Packet};
use embassy_net::{
    udp::{BindError, UdpSocket},
    IpAddress, IpEndpoint,
};
use embassy_time::{Duration, Instant};
use esp_println::println;

use crate::vec_in_myheap;

const ENABLE_LOGGING: bool = true;

// Add macro for logging
macro_rules! dhcp_log {
    ($($arg:tt)*) => {
        if ENABLE_LOGGING {
            println!($($arg)*);
        }
    };
}

pub const DHCP_BROADCAST: IpEndpoint = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), 68);
pub const DHCP_SERVER_ENDPOINT: IpEndpoint = IpEndpoint::new(IpAddress::v4(0, 0, 0, 0), 67);
pub const DHCP_BUFFER_SIZE: usize = 1024;
pub const DHCP_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 2, 1);

pub const DHCP_LEASE_TIME: Duration = Duration::from_secs(60 * 60 * 24);
pub const DHCP_LEASE_START: Ipv4Addr = Ipv4Addr::new(192, 168, 2, 2);
pub const DHCP_LEASE_END: Ipv4Addr = Ipv4Addr::new(192, 168, 2, 200);

#[derive(Debug, Clone)]
pub struct DhcpLease {
    pub ip: Ipv4Addr,
    pub mac: [u8; 16],
    pub expires: Instant,
}

pub struct DhcpLeaser {
    pub leases: heapless::Vec<DhcpLease, 4>,
}

impl DhcpLeaser {
    fn get_lease(&mut self, mac: [u8; 16]) -> Option<DhcpLease> {
        dhcp_log!("DhcpLeaser::get_lease called with mac: {:02x?}", mac);
        for lease in &self.leases {
            if lease.mac == mac {
                dhcp_log!("Lease found for mac: {:02x?} -> {:?}", mac, lease.ip);
                return Some(lease.clone());
            }
        }
        dhcp_log!("No lease found for mac: {:02x?}", mac);
        None
    }

    fn next_lease(&mut self) -> Option<Ipv4Addr> {
        dhcp_log!("DhcpLeaser::next_lease called");
        let start: u32 = DHCP_LEASE_START.into();
        let end: u32 = DHCP_LEASE_END.into();

        for ip in start..=end {
            let ip: Ipv4Addr = ip.into();
            let mut found = false;

            for lease in &self.leases {
                if lease.ip == ip {
                    found = true;
                }
            }

            if !found {
                dhcp_log!("Next available lease IP: {:?}", ip);
                return Some(ip);
            }
        }
        dhcp_log!("No available lease IPs");
        None
    }

    fn add_lease(&mut self, ip: Ipv4Addr, mac: [u8; 16], expires: Instant) -> bool {
        dhcp_log!("DhcpLeaser::add_lease called: ip={:?}, mac={:02x?}, expires={:?}", ip, mac, expires);
        self.remove_lease(mac);
        let res = self.leases.push(DhcpLease { ip, mac, expires }).is_ok();
        dhcp_log!("Lease added: {} (current leases: {})", res, self.leases.len());
        res
    }

    fn remove_lease(&mut self, mac: [u8; 16]) -> bool {
        dhcp_log!("DhcpLeaser::remove_lease called for mac: {:02x?}", mac);
        for (i, lease) in self.leases.iter().enumerate() {
            if lease.mac == mac {
                self.leases.remove(i);
                dhcp_log!("Lease removed for mac: {:02x?}", mac);
                return true;
            }
        }
        dhcp_log!("No lease to remove for mac: {:02x?}", mac);
        false
    }
}

pub struct DhcpServer<'a> {
    leaser: DhcpLeaser,
    sock: UdpSocket<'a>,
}

impl<'a> DhcpServer<'a> {
    pub fn new(mut sock: UdpSocket<'a>) -> Result<Self, BindError> {
        dhcp_log!("DhcpServer::new called, binding to {:?}", DHCP_SERVER_ENDPOINT);
        sock.bind(DHCP_SERVER_ENDPOINT)?;

        Ok(Self {
            leaser: DhcpLeaser {
                leases: Default::default(),
            },
            sock,
        })
    }

    pub async fn run(&mut self) {
        dhcp_log!("DhcpServer::run started");
        let mut vec = vec_in_myheap!(0u8; DHCP_BUFFER_SIZE);
        let buf: &mut [u8] = vec.as_mut_slice();
        loop {
            dhcp_log!("Waiting for DHCP packet...");
            let res = self.sock.recv_from(buf).await;
            if let Ok((n, _addr)) = res {
                dhcp_log!("Received {n} bytes from {_addr:?}");
                let res = Packet::decode(&buf[..n]);
                if let Ok(packet) = res {
                    dhcp_log!("Decoded DHCP packet: {:?}", packet);
                    self.process_packet(packet).await;
                } else {
                    dhcp_log!("Failed to decode DHCP packet: {:?}", res);
                }
            } else {
                dhcp_log!("Failed to receive DHCP packet: {:?}", res);
            }
        }
    }

    async fn process_packet(&mut self, packet: Packet<'_>) {
        dhcp_log!("DhcpServer::process_packet called");
        let Some(action) = self.get_packet_action(&packet) else {
            dhcp_log!("Skipping process_packet because packet action was None");
            return;
        };
        dhcp_log!("Processing packet action: {:?}", action);

        match action {
            Action::Discover(requested_ip, mac) => {
                dhcp_log!("Handling DHCP Discover: requested_ip={:?}, mac={:02x?}", requested_ip, mac);
                let ip = requested_ip
                    .and_then(|ip| {
                        let mac_lease = self.leaser.get_lease(*mac);
                        let available = mac_lease
                            .map(|d| d.ip == ip || Instant::now() > d.expires)
                            .unwrap_or(true);

                        available.then_some(ip)
                    })
                    .or_else(|| self.leaser.get_lease(*mac).map(|l| l.ip))
                    .or_else(|| self.leaser.next_lease());

                dhcp_log!("Offer IP: {:?}", ip);
                if ip.is_some() {
                    self.send_reply(packet, edge_dhcp::MessageType::Offer, ip)
                        .await;
                }
            }
            Action::Request(ip, mac) => {
                dhcp_log!("Handling DHCP Request: ip={:?}, mac={:02x?}", ip, mac);
                let mac_lease = self.leaser.get_lease(*mac);
                let available = mac_lease
                    .map(|d| d.ip == ip || Instant::now() > d.expires)
                    .unwrap_or(true);

                dhcp_log!("Lease available: {}", available);

                let ip = (available
                    && self
                        .leaser
                        .add_lease(ip, *mac, Instant::now() + DHCP_LEASE_TIME))
                .then_some(ip);

                let msg_type = match ip {
                    Some(_) => MessageType::Ack,
                    None => MessageType::Nak,
                };

                dhcp_log!("Sending reply: {:?} for ip={:?}", msg_type, ip);

                self.send_reply(packet, msg_type, ip).await;
            }
            Action::Release(_ip, mac) | Action::Decline(_ip, mac) => {
                dhcp_log!("Handling DHCP Release/Decline: mac={:02x?}", mac);
                self.leaser.remove_lease(*mac);
            }
        }
    }

    async fn send_reply(&mut self, packet: Packet<'_>, mt: MessageType, ip: Option<Ipv4Addr>) {
        dhcp_log!("DhcpServer::send_reply called: mt={:?}, ip={:?}", mt, ip);
        let mut opt_buf = Options::buf();

        let mut captive_portal: heapless::String<64> = heapless::String::new();
        let captive_portal =
            match core::fmt::write(&mut captive_portal, format_args!("http://{}", DHCP_IP)) {
                Ok(_) => Some(captive_portal.as_str()),
                Err(_) => None,
            };

        if let Some(url) = captive_portal {
            dhcp_log!("Captive portal URL: {url}");
        } else {
            dhcp_log!("Failed to generate captive portal URL");
        }

        // Build reply options, ensuring option 114 is present
        let mut reply_options = packet.options.reply(
            mt,
            DHCP_IP,
            DHCP_LEASE_TIME.as_secs() as u32,
            &[],
            None,
            &[],
            captive_portal,
            &mut opt_buf,
        );

        // Check if option 114 is present, if not, add it
        let has_114 = reply_options.iter().any(|opt| matches!(opt, DhcpOption::CaptiveUrl(_)));
        if !has_114 {
            if let Some(url) = captive_portal {
                // Option 114 is a string, so encode as bytes
                let reply_options = Options::new(reply_options.iter());
                dhcp_log!("Option 114 (Captive-Portal) forcibly added to reply options");
            }
        }

        let reply = packet.new_reply(
            ip,
            reply_options,
        );
        
        dhcp_log!("Sending DHCP reply: {:?}", reply);

        let mut vec = vec_in_myheap!(0u8; DHCP_BUFFER_SIZE);
        let buf: &mut [u8] = vec.as_mut_slice();
        let bytes_res = reply.encode(buf);
        match bytes_res {
            Ok(bytes) => {
                dhcp_log!("Encoded reply, sending {} bytes to {:?}", bytes.len(), DHCP_BROADCAST);
                let res = self.sock.send_to(bytes, DHCP_BROADCAST).await;
                if let Err(_e) = res {
                    dhcp_log!("Dhcp sock send error: {_e:?}");
                } else {
                    dhcp_log!("Dhcp reply sent successfully");
                }
            }
            Err(_e) => {
                dhcp_log!("Dhcp encode error: {_e:?}");
            }
        }
    }

    fn get_packet_action<'b>(&self, packet: &'b Packet<'b>) -> Option<Action<'b>> {
        dhcp_log!("DhcpServer::get_packet_action called");
        if packet.reply {
            dhcp_log!("Packet is a reply, ignoring");
            return None;
        }

        let message_type = packet.options.iter().find_map(|option| match option {
            DhcpOption::MessageType(msg_type) => Some(msg_type),
            _ => None,
        });

        let message_type = message_type.or_else(|| {
            dhcp_log!("Ignoring DHCP request, no message type found: {packet:?}");
            None
        })?;

        let server_identifier = packet.options.iter().find_map(|option| match option {
            DhcpOption::ServerIdentifier(ip) => Some(ip),
            _ => None,
        });

        if server_identifier.is_some() && server_identifier != Some(DHCP_IP) {
            dhcp_log!(
                "Ignoring {message_type} request, not addressed to this server: {packet:?}"
            );
            return None;
        }

        match message_type {
            MessageType::Discover => {
                dhcp_log!("Packet is DHCP Discover");
                Some(Action::Discover(
                    Self::get_requested_ip(&packet.options),
                    &packet.chaddr,
                ))
            }
            MessageType::Request => {
                dhcp_log!("Packet is DHCP Request");
                let requested_ip =
                    Self::get_requested_ip(&packet.options).or_else(|| {
                        match packet.ciaddr.is_unspecified() {
                            true => None,
                            false => Some(packet.ciaddr),
                        }
                    })?;

                Some(Action::Request(requested_ip, &packet.chaddr))
            }
            MessageType::Release if server_identifier == Some(DHCP_IP) => {
                dhcp_log!("Packet is DHCP Release");
                Some(Action::Release(packet.yiaddr, &packet.chaddr))
            }
            MessageType::Decline if server_identifier == Some(DHCP_IP) => {
                dhcp_log!("Packet is DHCP Decline");
                Some(Action::Decline(packet.yiaddr, &packet.chaddr))
            }
            _ => {
                dhcp_log!("Packet is unhandled message type: {:?}", message_type);
                None
            }
        }
    }

    fn get_requested_ip<'b>(options: &'b Options<'b>) -> Option<Ipv4Addr> {
        dhcp_log!("DhcpServer::get_requested_ip called");
        options.iter().find_map(|option| {
            if let DhcpOption::RequestedIpAddress(ip) = option {
                dhcp_log!("Requested IP found in options: {:?}", ip);
                Some(ip)
            } else {
                None
            }
        })
    }
}
