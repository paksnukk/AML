//! MIDI I/O: port listing, the [`MidiSink`] / [`MidiSource`] traits the
//! workers use, and their midir implementations.
//!
//! Device ports are opened inside the worker thread (as before the engine
//! existed); custom endpoints are handed over already constructed.

use crate::devices::DeviceSelector;
use anyhow::{anyhow, Result};
use std::any::Any;

/// Receives raw MIDI messages produced by the decoder.
pub trait MidiSink {
    /// Send one complete MIDI message (e.g. `[0x90, note, vel]`).
    fn send(&mut self, bytes: &[u8]) -> Result<()>;
}

impl MidiSink for midir::MidiOutputConnection {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        midir::MidiOutputConnection::send(self, bytes).map_err(|e| anyhow!("MIDI send: {}", e))
    }
}

/// Callback a [`MidiSource`] delivers raw incoming MIDI messages to.
pub type MidiCallback = Box<dyn FnMut(&[u8]) + Send + 'static>;

/// Keeps a MIDI input connected; dropping it disconnects.
pub struct MidiInputGuard(#[allow(dead_code)] Box<dyn Any>);

impl MidiInputGuard {
    /// Wrap whatever object keeps the connection alive.
    pub fn new<T: 'static>(connection: T) -> Self {
        Self(Box::new(connection))
    }
}

/// A MIDI input for the encoder: once connected it calls `on_message` for
/// every incoming message until the returned guard is dropped.
pub trait MidiSource: Send {
    fn connect(self: Box<Self>, on_message: MidiCallback) -> Result<MidiInputGuard>;
}

/// Where the decoder sends MIDI.
pub enum MidiOut {
    /// No MIDI output: decode to events only.
    None,
    /// A midir output port.
    Device(DeviceSelector),
    /// Any other sink (e.g. a recorder in tests).
    Custom(Box<dyn MidiSink + Send>),
}

/// Where the encoder reads MIDI from.
pub enum MidiIn {
    /// No MIDI input (the encoder then runs its simulated scale).
    None,
    /// A midir input port.
    Device(DeviceSelector),
    /// Any other source (e.g. injected messages in tests).
    Custom(Box<dyn MidiSource>),
}

fn port_names<T: midir::MidiIO>(io: &T) -> Vec<String> {
    io.ports()
        .iter()
        .map(|p| io.port_name(p).unwrap_or_else(|_| "Unknown".into()))
        .collect()
}

/// MIDI input port names in enumeration order (index = [`DeviceSelector::Index`]).
pub fn list_inputs() -> Result<Vec<String>> {
    let midi_in = midir::MidiInput::new("aml list in").map_err(|e| anyhow!("{}", e))?;
    Ok(port_names(&midi_in))
}

/// MIDI output port names in enumeration order (index = [`DeviceSelector::Index`]).
pub fn list_outputs() -> Result<Vec<String>> {
    let midi_out = midir::MidiOutput::new("aml list out").map_err(|e| anyhow!("{}", e))?;
    Ok(port_names(&midi_out))
}

fn port_index(selector: &DeviceSelector, dir: &str) -> Result<usize> {
    selector
        .index()
        .ok_or_else(|| anyhow!("No MIDI {} port selected", dir))
}

/// Name of the selected input port (errors if it does not exist).
pub fn input_port_name(selector: &DeviceSelector) -> Result<String> {
    let index = port_index(selector, "in")?;
    list_inputs()?
        .into_iter()
        .nth(index)
        .ok_or_else(|| anyhow!("MIDI in index {} not found", index))
}

/// Name of the selected output port (errors if it does not exist).
pub fn output_port_name(selector: &DeviceSelector) -> Result<String> {
    let index = port_index(selector, "out")?;
    list_outputs()?
        .into_iter()
        .nth(index)
        .ok_or_else(|| anyhow!("MIDI out index {} not found", index))
}

/// Connect to the selected output port.
pub fn open_output(selector: &DeviceSelector) -> Result<midir::MidiOutputConnection> {
    let index = port_index(selector, "out")?;
    let midi_out = midir::MidiOutput::new("aml decode out").map_err(|e| anyhow!("{}", e))?;
    let ports = midi_out.ports();
    let port = ports
        .get(index)
        .ok_or_else(|| anyhow!("MIDI out index {} not found", index))?;
    midi_out
        .connect(port, "aml out")
        .map_err(|e| anyhow!("MIDI connect: {}", e))
}

/// Connect to the selected input port; `on_message` gets every message.
pub fn open_input(
    selector: &DeviceSelector,
    mut on_message: MidiCallback,
) -> Result<midir::MidiInputConnection<()>> {
    let index = port_index(selector, "in")?;
    let midi_in = midir::MidiInput::new("aml encode in").map_err(|e| anyhow!("{}", e))?;
    let ports = midi_in.ports();
    let port = ports
        .get(index)
        .ok_or_else(|| anyhow!("MIDI in index {} not found", index))?;
    midi_in
        .connect(
            port,
            "aml in",
            move |_stamp, message, _| on_message(message),
            (),
        )
        .map_err(|e| anyhow!("MIDI connect: {}", e))
}
