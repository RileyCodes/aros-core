//! BLE module — Even Realities G2 smart glasses protocol
//!
//! G2 is a BLE display-only device (no Android, no compute).
//! This module handles the BLE protocol to push text/images to G2's display.
//!
//! Protocol: reverse-engineered at https://github.com/i-soxi/even-g2-protocol
//!
//! Architecture:
//!   Phone/Computer (runs aros-core) ←BLE→ Even G2 (displays text)
//!   vs INMO Air3 where aros-core runs on-device
//!
//! Key differences from Android HAL:
//! - No AudioRecord (mic on phone/computer, not glasses)
//! - No Camera (G2 has no camera)
//! - Display via BLE protocol, not Android Views
//! - Engine runs on companion device, not glasses

/// G2 display capabilities
pub struct G2Display {
    /// Max text length per screen
    pub max_chars: usize,
    /// Display is monochrome green MicroLED
    pub is_monochrome: bool,
}

impl Default for G2Display {
    fn default() -> Self {
        Self {
            max_chars: 200, // estimated, depends on font size
            is_monochrome: true,
        }
    }
}

/// G2 BLE protocol commands (from reverse-engineered spec)
#[derive(Debug)]
pub enum G2Command {
    /// Show text on display
    ShowText {
        text: String,
        position: TextPosition,
    },
    /// Clear display
    Clear,
    /// Show notification
    Notify { title: String, body: String },
}

#[derive(Debug, Clone, Copy)]
pub enum TextPosition {
    Center,
    Top,
    Bottom,
}

/// Placeholder for BLE connection (will use btleplug or Android BLE API)
pub struct G2Connection {
    connected: bool,
}

impl G2Connection {
    pub fn new() -> Self {
        Self { connected: false }
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Connect to G2 by name (placeholder)
    pub async fn connect(&mut self, _device_name: &str) -> Result<(), String> {
        // TODO: Implement with btleplug crate
        // 1. Scan for BLE devices matching "Even G2"
        // 2. Connect to GATT server
        // 3. Discover services/characteristics
        // 4. Subscribe to notifications
        log::info!("G2: connect placeholder (not implemented yet)");
        Err("G2 BLE not implemented yet".to_string())
    }

    /// Send command to G2
    pub async fn send(&self, _cmd: G2Command) -> Result<(), String> {
        if !self.connected {
            return Err("Not connected".to_string());
        }
        // TODO: Encode command to BLE protocol bytes and write to characteristic
        Ok(())
    }
}

impl Default for G2Connection {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_g2_display_default() {
        let d = G2Display::default();
        assert!(d.is_monochrome);
        assert!(d.max_chars > 0);
    }

    #[test]
    fn test_g2_connection_default() {
        let conn = G2Connection::new();
        assert!(!conn.is_connected());
    }
}
