use phm_icd::{ToMcu, ToMcuI2c, ToMcuSpi, ToPc, ToPcI2c, ToPcSpi};
use postcard::{to_stdvec_cobs, CobsAccumulator, FeedResult};
use serialport::SerialPort;
use std::{
    io::{self, ErrorKind},
    time::{Duration, Instant},
};

/// The Pretty HAL Machine
///
/// This wraps a serial port connection to an embedded machine,
/// and implements various [embedded-hal](embedded-hal) traits.
pub struct Machine {
    port: Box<dyn SerialPort>,
    cobs_buf: CobsAccumulator<512>,
    command_timeout: Duration,
}

/// The main Error type
#[derive(Debug)]
pub enum Error {
    PhmSerial(io::Error),
    Postcard(postcard::Error),
    Timeout(Duration),

    // TODO: This probably needs some more context/nuance...
    ResponseError,
    InvalidParameter,
    Unknown,
}

impl From<postcard::Error> for Error {
    fn from(err: postcard::Error) -> Self {
        Error::Postcard(err)
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::PhmSerial(err)
    }
}

impl Machine {
    pub fn from_port(port: Box<dyn SerialPort>) -> Result<Self, Error> {
        // TODO: some kind of sanity checking? Check version, protocol,
        // signs of life, anything?
        Ok(Self {
            port,
            cobs_buf: CobsAccumulator::new(),
            command_timeout: Duration::from_secs(3),
        })
    }

    /// Set the timeout for a full command to complete.
    ///
    /// This is not a single message timeout, but rather the timeout
    /// for a whole command (e.g. an I2C write) to execute. This is currently
    /// only checked/set host side, so endless loops on the embedded side are
    /// still possible.
    pub fn set_command_timeout(&mut self, timeout: Duration) {
        self.command_timeout = timeout;
    }

    fn poll(&mut self) -> Result<Vec<ToPc>, Error> {
        let mut responses = vec![];
        let mut buf = [0u8; 1024];

        // read from stdin and push it to the decoder
        match self.port.read(&mut buf) {
            Ok(n) if n > 0 => {
                let buf = &buf[..n];
                let mut window = &buf[..];

                'cobs: while !window.is_empty() {
                    window = match self.cobs_buf.feed::<Result<phm_icd::ToPc, ()>>(&window) {
                        FeedResult::Consumed => break 'cobs,
                        FeedResult::OverFull(new_wind) => new_wind,
                        FeedResult::DeserError(new_wind) => new_wind,
                        FeedResult::Success { data, remaining } => {
                            // Do something with `data: MyData` here.
                            if let Ok(data) = data {
                                responses.push(data);
                            } else {
                                return Err(Error::ResponseError);
                            }

                            remaining
                        }
                    };
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::TimedOut => {}
            Err(e) => {
                return Err(Error::PhmSerial(e));
            }
        }
        Ok(responses)
    }
}

impl embedded_hal::blocking::i2c::Write for Machine {
    type Error = Error;

    fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), Error> {
        let msg = ToMcu::I2c(ToMcuI2c::Write {
            addr: address,
            output: bytes.iter().cloned().collect(),
        });
        let ser_msg = to_stdvec_cobs(&msg)?;
        self.port.write_all(&ser_msg)?;

        let start = Instant::now();

        while start.elapsed() < self.command_timeout {
            for msg in self.poll()? {
                if let ToPc::I2c(ToPcI2c::WriteComplete { addr }) = msg {
                    if address != addr {
                        continue;
                    }
                    return Ok(());
                }
            }

            // TODO: We should probably just use the `timeout` value of the serial
            // port, (e.g. don't delay at all), but I guess this is fine for now.
            std::thread::sleep(Duration::from_millis(10));
        }

        Err(Error::Timeout(self.command_timeout))
    }
}

impl embedded_hal::blocking::i2c::Read for Machine {
    type Error = Error;

    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        let msg = ToMcu::I2c(ToMcuI2c::Read {
            addr: address,
            to_read: len_to_u32(buffer.len())?,
        });
        let ser_msg = to_stdvec_cobs(&msg)?;
        self.port.write_all(&ser_msg)?;

        let start = Instant::now();

        while start.elapsed() < self.command_timeout {
            for msg in self.poll()? {
                if let ToPc::I2c(ToPcI2c::Read { addr, data_read }) = msg {
                    if address != addr {
                        continue;
                    }

                    if data_read.len() != buffer.len() {
                        return Err(Error::ResponseError);
                    } else {
                        buffer.copy_from_slice(&data_read);
                        return Ok(());
                    }
                }
            }

            // TODO: We should probably just use the `timeout` value of the serial
            // port, (e.g. don't delay at all), but I guess this is fine for now.
            std::thread::sleep(Duration::from_millis(10));
        }

        Err(Error::Timeout(self.command_timeout))
    }
}

impl embedded_hal::blocking::i2c::WriteRead for Machine {
    type Error = Error;

    fn write_read(
        &mut self,
        address: u8,
        bytes: &[u8],
        buffer: &mut [u8],
    ) -> Result<(), Self::Error> {
        let msg = ToMcu::I2c(ToMcuI2c::WriteThenRead {
            addr: address,
            output: bytes.iter().cloned().collect(),
            to_read: len_to_u32(buffer.len())?,
        });
        let ser_msg = to_stdvec_cobs(&msg)?;
        self.port.write_all(&ser_msg)?;

        let start = Instant::now();

        while start.elapsed() < self.command_timeout {
            for msg in self.poll()? {
                if let ToPc::I2c(ToPcI2c::WriteThenRead { addr, data_read }) = msg {
                    if address != addr {
                        continue;
                    }

                    if data_read.len() != buffer.len() {
                        return Err(Error::ResponseError);
                    } else {
                        buffer.copy_from_slice(&data_read);
                        return Ok(());
                    }
                }
            }

            // TODO: We should probably just use the `timeout` value of the serial
            // port, (e.g. don't delay at all), but I guess this is fine for now.
            std::thread::sleep(Duration::from_millis(10));
        }

        Err(Error::Timeout(self.command_timeout))
    }
}

impl embedded_hal::blocking::spi::Write<u8> for Machine {
    type Error = Error;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let msg = ToMcu::Spi(ToMcuSpi::Write {
            output: bytes.iter().cloned().collect(),
        });
        let ser_msg = to_stdvec_cobs(&msg)?;
        self.port.write_all(&ser_msg)?;

        let start = Instant::now();

        while start.elapsed() < self.command_timeout {
            for msg in self.poll()? {
                if let ToPc::Spi(ToPcSpi::WriteComplete) = msg {
                    return Ok(());
                }
            }

            // TODO: We should probably just use the `timeout` value of the serial
            // port, (e.g. don't delay at all), but I guess this is fine for now.
            std::thread::sleep(Duration::from_millis(10));
        }

        Err(Error::Timeout(self.command_timeout))
    }
}

impl embedded_hal::blocking::spi::Transfer<u8> for Machine {
    type Error = Error;

    fn transfer<'a>(&mut self, buffer: &'a mut [u8]) -> Result<&'a [u8], Self::Error> {
        let msg = ToMcu::Spi(ToMcuSpi::Transfer {
            output: buffer.iter().cloned().collect(),
        });
        let ser_msg = to_stdvec_cobs(&msg)?;
        self.port.write_all(&ser_msg)?;

        let start = Instant::now();

        while start.elapsed() < self.command_timeout {
            for msg in self.poll()? {
                if let ToPc::Spi(ToPcSpi::Transfer { data_read }) = msg {
                    if data_read.len() != buffer.len() {
                        return Err(Error::ResponseError);
                    } else {
                        buffer.copy_from_slice(&data_read);
                        return Ok(buffer);
                    }
                }
            }

            // TODO: We should probably just use the `timeout` value of the serial
            // port, (e.g. don't delay at all), but I guess this is fine for now.
            std::thread::sleep(Duration::from_millis(10));
        }

        Err(Error::Timeout(self.command_timeout))
    }
}

// TODO: This is overly accepting! We have a much smaller max message size than this.
fn len_to_u32(len: usize) -> Result<u32, Error> {
    len.try_into().map_err(|_| Error::InvalidParameter)
}
