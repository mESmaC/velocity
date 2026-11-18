//! The application-wide zoom preference and its available adjustments.

use serde::{Deserialize, Serialize};

const MINIMUM: u16 = 90;
const DEFAULT: u16 = 100;
const MAXIMUM: u16 = 150;
const STEP: u16 = 10;

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiZoom(u16);

impl Default for UiZoom {
    fn default() -> Self {
        Self(DEFAULT)
    }
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Deserialize)]
pub enum UiZoomAdjustment {
    Increase,
    Decrease,
    Reset,
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Serialize)]
pub struct UiZoomView {
    pub percent: u16,
    pub can_increase: bool,
    pub can_decrease: bool,
    pub can_reset: bool,
}

impl UiZoom {
    fn percent(self) -> u16 {
        self.0.clamp(MINIMUM, MAXIMUM)
    }

    pub fn adjust(self, adjustment: UiZoomAdjustment) -> Self {
        let percent = self.percent();
        Self(match adjustment {
            UiZoomAdjustment::Increase => (percent + STEP).min(MAXIMUM),
            UiZoomAdjustment::Decrease => (percent - STEP).max(MINIMUM),
            UiZoomAdjustment::Reset => DEFAULT,
        })
    }

    pub fn view(self) -> UiZoomView {
        let percent = self.percent();
        UiZoomView {
            percent,
            can_increase: percent < MAXIMUM,
            can_decrease: percent > MINIMUM,
            can_reset: percent != DEFAULT,
        }
    }
}
