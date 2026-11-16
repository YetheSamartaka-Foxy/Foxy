use super::{fields::Arma3FieldKind as K, optional_fields};
use serde_json::Number;

const U32: u64 = u32::MAX as u64;

optional_fields!(Arma3BasicSettings {
    min_bandwidth: u64 => ("MinBandwidth", K::Integer { min: 1, max: u64::MAX }, "Minimum bandwidth (bits per second)", 0),
    max_bandwidth: u64 => ("MaxBandwidth", K::Integer { min: 1, max: u64::MAX }, "Maximum bandwidth (bits per second)", 0),
    max_msg_send: u32 => ("MaxMsgSend", K::Integer { min: 1, max: U32 }, "Messages per simulation cycle", 0),
    max_size_guaranteed: u32 => ("MaxSizeGuaranteed", K::Integer { min: 1, max: U32 }, "Guaranteed payload (bytes)", 0),
    max_size_nonguaranteed: u32 => ("MaxSizeNonguaranteed", K::Integer { min: 1, max: U32 }, "Nonguaranteed payload (bytes)", 0),
    min_error_to_send: Number => ("MinErrorToSend", K::Decimal { min: 0.0, max: f64::MAX }, "Network update error threshold", 0),
    min_error_to_send_near: Number => ("MinErrorToSendNear", K::Decimal { min: 0.0, max: f64::MAX }, "Nearby network update error threshold", 0),
    max_custom_file_size: u32 => ("MaxCustomFileSize", K::Integer { min: 0, max: U32 }, "Custom face/sound limit (bytes)", 0),
    sockets: Box<Arma3SocketSettings> => ("sockets", K::Section(Arma3SocketSettings::FIELDS), "Network socket settings", 0),
});

optional_fields!(Arma3SocketSettings {
    max_packet_size: u32 => ("maxPacketSize", K::Integer { min: 1, max: 65507 }, "Socket packet ceiling (bytes)", 0),
});

impl Arma3BasicSettings {
    pub fn validate_relationships(&self) -> Result<(), String> {
        self.validate()?;
        if let (Some(min), Some(max)) = (self.min_bandwidth, self.max_bandwidth)
            && min > max
        {
            return Err("Arma 3 MinBandwidth cannot exceed MaxBandwidth".into());
        }
        if let Some(packet) = self
            .sockets
            .as_ref()
            .and_then(|sockets| sockets.max_packet_size)
            && [self.max_size_guaranteed, self.max_size_nonguaranteed]
                .into_iter()
                .flatten()
                .any(|size| size > packet)
        {
            return Err("Packet payload sizes cannot exceed the socket packet ceiling".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bandwidth_and_packet_relationships_are_checked() {
        let inverted = Arma3BasicSettings {
            min_bandwidth: Some(2_000_000),
            max_bandwidth: Some(1_000_000),
            ..Default::default()
        };
        assert!(inverted.validate_relationships().is_err());
        let oversized = Arma3BasicSettings {
            max_size_guaranteed: Some(1401),
            sockets: Some(Box::new(Arma3SocketSettings {
                max_packet_size: Some(1400),
            })),
            ..Default::default()
        };
        assert!(oversized.validate_relationships().is_err());
        let zero = Arma3BasicSettings {
            max_msg_send: Some(0),
            ..Default::default()
        };
        assert!(zero.validate_relationships().is_err());
        let empty_sockets = Arma3BasicSettings {
            sockets: Some(Box::default()),
            ..Default::default()
        };
        assert_eq!(empty_sockets.render().unwrap(), "class sockets\n{\n};\n");
    }
}
