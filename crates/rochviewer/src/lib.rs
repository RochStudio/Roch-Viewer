// Roch Viewer -- a read-only memory-controller and timing viewer.
// Copyright (C) 2026 Roch Studio
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Roch Viewer: a read-only memory-controller and timing viewer.
//!
//! Laid out by what a module talks to rather than by layer: hardware is the
//! access itself, intel the memory controller, memory the modules, sensors
//! the board, gpu the card. The dispatcher and the profile it dispatches on
//! stay here, because they are what decides which of the rest is allowed to
//! run. The window lives in the `roch-viewer-app` crate.

pub mod memo;
pub mod pyfmt;
pub mod wmi_util;

pub mod paths;
pub mod version;

pub mod display_values;
pub mod row;

pub mod platform_profiles;
pub mod system_identity;
pub mod timings;
pub mod unsupported_profile;

pub mod amd;
pub mod gpu;
pub mod hardware;
pub mod intel;
pub mod memory;
pub mod sensors;

#[cfg(test)]
pub mod testing;
