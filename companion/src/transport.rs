/// Abstraction over the wireless/simulated link to the physical (or virtual) desk controller.
pub trait DeviceTransport {
    fn label(&self) -> String;
    fn is_connected(&self) -> bool;
    fn send_packet(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn poll_events(&mut self) -> Vec<Vec<u8>>;
}

/// Live packet and byte counters for the active transport link.
#[derive(Debug, Default, Clone)]
pub struct TransportStats {
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub last_tx_summary: Option<String>,
    pub last_rx_summary: Option<String>,
}

/// Fallback transport used when running strictly in local terminal mode without `--simulate`.
#[derive(Debug, Default)]
pub struct NoopTransport;

impl DeviceTransport for NoopTransport {
    fn label(&self) -> String {
        "Local terminal only".to_string()
    }

    fn is_connected(&self) -> bool {
        false
    }

    fn send_packet(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }

    fn poll_events(&mut self) -> Vec<Vec<u8>> {
        Vec::new()
    }
}
