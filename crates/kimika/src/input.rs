//! the keyboard as the console's buttons and sticks, by where the keys sit:
//! Z, X, A and S the face buttons, Q and W the shoulders, the arrows the
//! directional buttons, and I, J, K and L the left stick.

use kimika_core::hle::ctrl;
use kimika_core::CtrlState;
use winit::keyboard::{KeyCode, PhysicalKey};

/// what keys are held between frames.
#[derive(Default)]
pub struct Keyboard {
    buttons: u32,
    /// the left stick's keys held: up, down, left, right.
    stick: [bool; 4],
}

const BUTTONS: [(KeyCode, u32); 12] = [
    (KeyCode::KeyZ, ctrl::CROSS),
    (KeyCode::KeyX, ctrl::CIRCLE),
    (KeyCode::KeyA, ctrl::SQUARE),
    (KeyCode::KeyS, ctrl::TRIANGLE),
    (KeyCode::KeyQ, ctrl::L1),
    (KeyCode::KeyW, ctrl::R1),
    (KeyCode::Enter, ctrl::START),
    (KeyCode::Backspace, ctrl::SELECT),
    (KeyCode::ArrowUp, ctrl::UP),
    (KeyCode::ArrowDown, ctrl::DOWN),
    (KeyCode::ArrowLeft, ctrl::LEFT),
    (KeyCode::ArrowRight, ctrl::RIGHT),
];

const STICK: [KeyCode; 4] = [KeyCode::KeyI, KeyCode::KeyK, KeyCode::KeyJ, KeyCode::KeyL];

impl Keyboard {
    pub fn key(&mut self, key: PhysicalKey, pressed: bool) {
        let PhysicalKey::Code(code) = key else { return };
        for (bound, button) in BUTTONS {
            if bound == code {
                if pressed {
                    self.buttons |= button;
                } else {
                    self.buttons &= !button;
                }
            }
        }
        for (held, bound) in self.stick.iter_mut().zip(STICK) {
            if bound == code {
                *held = pressed;
            }
        }
    }

    /// lets go of everything, when the window loses the keyboard.
    pub fn release(&mut self) {
        *self = Keyboard::default();
    }

    pub fn state(&self) -> CtrlState {
        // two opposite keys held cancel out, as a real stick would
        let axis = |minus: bool, plus: bool| (128 + 127 * plus as i32 - 128 * minus as i32) as u8;
        let [up, down, left, right] = self.stick;
        CtrlState { buttons: self.buttons, lx: axis(left, right), ly: axis(up, down), ..CtrlState::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_hold_buttons_and_tilt_the_stick() {
        let mut keyboard = Keyboard::default();
        keyboard.key(PhysicalKey::Code(KeyCode::KeyZ), true);
        keyboard.key(PhysicalKey::Code(KeyCode::KeyJ), true);
        let state = keyboard.state();
        assert_eq!(state.buttons, ctrl::CROSS);
        assert_eq!((state.lx, state.ly), (0, 128));
        keyboard.key(PhysicalKey::Code(KeyCode::KeyL), true);
        assert_eq!(keyboard.state().lx, 127);
        keyboard.release();
        assert_eq!(keyboard.state(), CtrlState::default());
    }
}
