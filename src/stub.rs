use alloc::format;
use alloc::vec::Vec;

use log::*;

use crate::{
    protocol::{Command, Packet, ResponseWriter},
    Connection, Error, Target, TargetState,
};

enum ExecState {
    Paused,
    Running,
    Exit,
}

/// [`GdbStub`] maintains the state of a GDB remote debugging session, including
/// the underlying transport.
pub struct GdbStub<T: Target, C: Connection> {
    conn: C,
    exec_state: ExecState,
    _target: core::marker::PhantomData<T>,
}

impl<T: Target, C: Connection> GdbStub<T, C> {
    pub fn new(conn: C) -> GdbStub<T, C> {
        GdbStub {
            conn,
            exec_state: ExecState::Paused,
            _target: core::marker::PhantomData,
        }
    }

    fn handle_command(
        &mut self,
        target: &mut T,
        command: Command,
    ) -> Result<(), Error<T::Error, C::Error>> {
        // Acknowledge the command
        self.conn.write(b'+').map_err(Error::Connection)?;

        let mut res = ResponseWriter::new(&mut self.conn);

        match command {
            Command::QSupported(_features) => {
                // TODO: actually respond with own feature set
            }
            Command::H { .. } => {
                // TODO: implement me
                res.write_str("OK").map_err(Error::Connection)?;
            }
            Command::Unknown => trace!("Unknown command"),
            c => trace!("Unimplemented command: {:#?}", c),
        }

        res.flush().map_err(Error::Connection)
    }

    fn recv_packet<'a, 'b>(
        &'a mut self,
        packet_buffer: &'b mut Vec<u8>,
    ) -> Result<Option<Packet<'b>>, Error<T::Error, C::Error>> {
        let header_byte = match self.exec_state {
            // block waiting for a gdb command
            ExecState::Paused => self.conn.read().map(Some),
            ExecState::Running => self.conn.read_nonblocking(),
            ExecState::Exit => unreachable!(),
        };

        match header_byte {
            Ok(None) => Ok(None), // no incoming message
            Ok(Some(header_byte)) => {
                packet_buffer.clear();
                packet_buffer.push(header_byte);
                if header_byte == b'$' {
                    // read the packet body
                    loop {
                        match self.conn.read().map_err(Error::Connection)? {
                            b'#' => break,
                            x => packet_buffer.push(x),
                        }
                    }
                    // append the # char
                    packet_buffer.push(b'#');
                    // and finally, read the checksum as well
                    packet_buffer.push(self.conn.read().map_err(Error::Connection)?);
                    packet_buffer.push(self.conn.read().map_err(Error::Connection)?);
                }

                Some(Packet::from_buf(packet_buffer))
                    .transpose()
                    .map_err(|e| Error::PacketParse(format!("{:?}", e)))
            }
            Err(e) => Err(Error::Connection(e)),
        }
    }

    /// Runs the target in a loop, with debug checks between each call to `target.step()`
    pub fn run(&mut self, target: &mut T) -> Result<TargetState, Error<T::Error, C::Error>> {
        let mut packet_buffer = Vec::new();
        let mut mem_accesses = Vec::new();

        loop {
            // Handle any incoming GDB packets
            match self.recv_packet(&mut packet_buffer)? {
                None => {}
                Some(packet) => match packet {
                    Packet::Ack => {}
                    Packet::Nack => unimplemented!(),
                    Packet::Command(command) => {
                        self.handle_command(target, command)?;
                    }
                },
            };

            match self.exec_state {
                ExecState::Paused => {}
                ExecState::Running => {
                    let target_state = target
                        .step(|access| mem_accesses.push(access))
                        .map_err(Error::TargetError)?;

                    if target_state == TargetState::Halted {
                        return Ok(TargetState::Halted);
                    };
                }
                ExecState::Exit => {
                    return Ok(TargetState::Running);
                }
            }
        }
    }
}
