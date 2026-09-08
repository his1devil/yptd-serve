//! Border *geometry*, kept deliberately apart from border *color*.
//!
//! Shape changes the glyph set a widget draws, which is layout, not styling.
//! Folding it into the highlight groups would let a color tweak silently
//! change a pane's inner area.

use ratatui::widgets::BorderType;

macro_rules! define_surfaces {
    ($($variant:ident => ($name:literal, $fallback:expr),)*) => {
        /// A framed surface whose border shape can be configured.
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(usize)]
        pub enum BorderSurface {
            $($variant,)*
        }

        impl BorderSurface {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];
            pub const COUNT: usize = Self::ALL.len();

            /// The key used under `[ui.border]`.
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|s| s.name() == name)
            }

            /// Shape used when neither this surface nor `default` is configured.
            pub(crate) fn built_in(self) -> BorderType {
                match self {
                    $(Self::$variant => $fallback,)*
                }
            }
        }
    };
}

define_surfaces! {
    Default  => ("default",  BorderType::Plain),
    Pane     => ("pane",     BorderType::Plain),
    Composer => ("composer", BorderType::Rounded),
    Modal    => ("modal",    BorderType::Plain),
    Picker   => ("picker",   BorderType::Plain),
    Message  => ("message",  BorderType::Rounded),
    Login    => ("login",    BorderType::Plain),
}

/// Parses a `[ui.border]` shape name.
pub fn parse_shape(value: &str) -> Option<BorderType> {
    Some(match value.trim() {
        "plain" => BorderType::Plain,
        "rounded" => BorderType::Rounded,
        "double" => BorderType::Double,
        "thick" => BorderType::Thick,
        "quadrant_inside" => BorderType::QuadrantInside,
        "quadrant_outside" => BorderType::QuadrantOutside,
        _ => return None,
    })
}

/// Every shape name a config may use, for error messages and docs.
pub const SHAPE_NAMES: &[&str] = &[
    "plain",
    "rounded",
    "double",
    "thick",
    "quadrant_inside",
    "quadrant_outside",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_names_round_trip_and_index_in_order() {
        for (index, surface) in BorderSurface::ALL.iter().enumerate() {
            assert_eq!(*surface as usize, index);
            assert_eq!(BorderSurface::from_name(surface.name()), Some(*surface));
        }
    }

    #[test]
    fn every_documented_shape_parses() {
        for name in SHAPE_NAMES {
            assert!(parse_shape(name).is_some(), "{name} should parse");
        }
        assert!(parse_shape("hairline").is_none());
    }

    #[test]
    fn composer_and_message_default_to_rounded() {
        assert_eq!(BorderSurface::Composer.built_in(), BorderType::Rounded);
        assert_eq!(BorderSurface::Message.built_in(), BorderType::Rounded);
        assert_eq!(BorderSurface::Pane.built_in(), BorderType::Plain);
    }
}
