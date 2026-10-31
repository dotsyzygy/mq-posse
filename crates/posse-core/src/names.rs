//! Names and text the plugin needs, with no game behind them.

use std::fmt;

const MAX_POSSE_NAME: usize = 32;

const COMPASS: [&str; 16] = ["N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW"];

/// A posse name that can't be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPosseName;

impl fmt::Display for InvalidPosseName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a posse name is 1 to {MAX_POSSE_NAME} letters, digits, - or _")
    }
}

pub fn validate_posse_name(name: &str) -> Result<(), InvalidPosseName> {
    let valid = (1..=MAX_POSSE_NAME).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid { Ok(()) } else { Err(InvalidPosseName) }
}

/// The name of a posse's shared memory. Windows matches these names by case,
/// so a posse's name is lowered: "Raid" and "raid" are one posse.
pub fn mapping_name(server: &str, posse: &str) -> String {
    let server: String = server.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    format!("Local\\MQ2Posse-{server}-{}", posse.to_ascii_lowercase())
}

/// The 16-point compass name of a heading in degrees clockwise from north, as
/// `${Me.Heading}` shows it.
pub fn compass(degrees: f32) -> &'static str {
    COMPASS[((degrees / 22.5 + 0.5) as usize) % COMPASS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posse_names() {
        for good in ["default", "Raid-1", "a_b", &"x".repeat(MAX_POSSE_NAME)] {
            assert_eq!(validate_posse_name(good), Ok(()), "{good}");
        }
        for bad in ["", "two words", "a\\b", "a/b", "é", &"x".repeat(MAX_POSSE_NAME + 1)] {
            assert_eq!(validate_posse_name(bad), Err(InvalidPosseName), "{bad}");
        }
    }

    #[test]
    fn mapping_names_are_one_object_per_server_and_posse() {
        assert_eq!(mapping_name("ProFusion EQ", "Raid"), "Local\\MQ2Posse-profusion_eq-raid");
        assert_eq!(mapping_name("a", "RAID"), mapping_name("A", "raid"));
        assert_ne!(mapping_name("a", "raid"), mapping_name("b", "raid"));
        assert!(!mapping_name("a\\b", "c").trim_start_matches("Local\\").contains('\\'), "a backslash would make it a namespace");
    }

    #[test]
    fn compass_points() {
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(11.0), "N");
        assert_eq!(compass(12.0), "NNE");
        assert_eq!(compass(90.0), "E");
        assert_eq!(compass(180.0), "S");
        assert_eq!(compass(359.9), "N");
    }
}
