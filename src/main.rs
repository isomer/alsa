fn generate_data(buffer: &mut [f32], rate: f32, phase: &mut f32) {
    const FREQUENCY: f32 = 440.0;
    for i in buffer {
        *i = (*phase * std::f32::consts::TAU * FREQUENCY / rate).sin();
        *phase += 1.0;
    }
    if *phase > rate {
        *phase -= rate;
    }
}

pub struct AlsaPlayback {
    pcm: alsa::PCM,
    async_fd: tokio::io::unix::AsyncFd<std::os::fd::RawFd>,
    poll_fd: libc::pollfd,
    rate: f32,
}

impl AlsaPlayback {
    pub fn new(device: &str) -> Self {
        let pcm = alsa::PCM::new(device, alsa::Direction::Playback, true)
            .expect("Failed to open device for playback");

        let hwparams = alsa::pcm::HwParams::any(&pcm).unwrap();
        hwparams
            .set_access(alsa::pcm::Access::RWInterleaved)
            .unwrap();
        hwparams.set_format(alsa::pcm::Format::FloatLE).unwrap();

        hwparams
            .set_rate_near(44100, alsa::ValueOr::Nearest)
            .unwrap();

        hwparams.set_channels(1).unwrap();

        pcm.hw_params(&hwparams).expect("Failed to initialise ALSA");

        let rate = hwparams.get_rate().expect("Couldn't get rate") as f32;

        drop(hwparams);

        let fds = alsa::poll::Descriptors::get(&pcm).expect("Couldn't get ALSA PCM FDs");
        let poll_fd = fds.first().unwrap();
        let async_fd = tokio::io::unix::AsyncFd::new(poll_fd.fd).expect("couldn't get async fd");

        Self {
            pcm,
            async_fd,
            poll_fd: *poll_fd,
            rate,
        }
    }

    #[inline]
    fn get_rate(&self) -> f32 {
        self.rate
    }

    fn get_interest(&self) -> tokio::io::Interest {
        use tokio::io::Interest;

        // Even for write only use like this, alsa often requires read events, since it's asking
        // you to wait on a status pipe rather than the underlying audio device.

        if self.poll_fd.events & libc::POLLIN != 0 {
            Interest::READABLE
        } else if self.poll_fd.events & libc::POLLOUT != 0 {
            Interest::WRITABLE
        } else if self.poll_fd.events & libc::POLLERR != 0 {
            Interest::ERROR
        } else {
            panic!("Unknown interest");
        }
    }
}

impl std::fmt::Debug for AlsaPlayback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut output = alsa::Output::buffer_open().expect("couldn't open output");
        self.pcm.dump(&mut output).expect("dump failed");
        f.write_str(&format!("{output}"))
    }
}

pub struct AlsaWriter<'p, Sample>(&'p AlsaPlayback, alsa::pcm::IO<'p, Sample>)
where
    Sample: alsa::pcm::IoFormat;

impl<'p, Sample: alsa::pcm::IoFormat> AlsaWriter<'p, Sample> {
    pub fn new(playback: &'p AlsaPlayback) -> Self {
        Self(playback, playback.pcm.io_checked().expect("Wrong format"))
    }

    pub async fn write(&self, to_send: &[Sample]) -> std::io::Result<usize> {
        let interest = self.0.get_interest();
        let mut guard = self
            .0
            .async_fd
            .ready(interest)
            .await
            .expect("Failed to get asyncfd guard");
        let io_result = guard.try_io(|_fd| {
            // As this is an example program for async i/o only, we are not handling XRUN or other
            // failures, just aborting to keep the code clear to understand the primary point.

            //let current_state = pcm.state();
            //assert_eq!(current_state, alsa::pcm::State::Running);

            let fds = [libc::pollfd {
                fd: self.0.poll_fd.fd,
                events: self.0.poll_fd.events,
                revents: if interest.is_readable() {
                    libc::POLLIN
                } else {
                    0
                } | if interest.is_writable() {
                    libc::POLLOUT
                } else {
                    0
                } | if interest.is_error() {
                    libc::POLLERR
                } else {
                    0
                },
            }];

            // Since ALSA may have asked for a POLLIN event for us to write (since it's actually
            // waiting on a status pipe), we need to remap that back to OUT, some alsa plugins
            // rely on this to perform some internal book keeping updates.  This does that.
            let flags = alsa::poll::Descriptors::revents(&self.0.pcm, &fds)
                .expect("Failed to alsa revents");

            self.0
                .pcm
                .avail_update()
                .expect("Failed to update ALSA avail");

            let delay = self.0.pcm.delay().expect("couldn't get delay");
            let rate = self.0.get_rate();
            let delay_ms = 1000.0 * delay as f32 / rate;

            println!("flags={flags:?}  delay={delay_ms}ms");
            if flags.contains(alsa::poll::Flags::OUT) {
                let frames = self.0.pcm.avail().unwrap();
                let count = self
                    .1
                    .writei(&to_send[..std::cmp::min(frames as usize, to_send.len())])
                    .expect("write failed");
                println!("{count}");
                Ok(count)
            } else {
                // ALSA is NOT ready for writing according to its internal logic (alsa_flags).
                // Return WouldBlock to prevent the spin: this tells Tokio to re-poll the FD.
                Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "ALSA not ready for write according to its revents flags",
                ))
            }
        });

        match io_result {
            Ok(Ok(count)) => Ok(count),
            Ok(Err(err)) => Err(err),
            Err(_would_block) => Ok(0),
        }
    }
}

const BUFFER_SIZE: usize = 65536;

pub struct AlsaBufferedWriter<'p, Sample>
where
    Sample: alsa::pcm::IoFormat,
{
    writer: AlsaWriter<'p, Sample>,
    buffer: std::collections::VecDeque<Sample>,
}

impl<'p, Sample> AlsaBufferedWriter<'p, Sample>
where
    Sample: alsa::pcm::IoFormat,
{
    pub fn new(writer: AlsaWriter<'p, Sample>) -> Self {
        Self {
            writer,
            buffer: Default::default(),
        }
    }

    pub async fn ready(&mut self) -> std::io::Result<()> {
        while self.buffer.len() >= BUFFER_SIZE {
            let (to_send, _) = self.buffer.as_slices();
            let count = self.writer.write(to_send).await?;
            self.buffer.drain(..count);
        }

        Ok(())
    }

    pub fn send(&mut self, sample: Sample) -> std::io::Result<()> {
        self.buffer.push_back(sample);
        Ok(())
    }

    pub async fn flush(&mut self) -> std::io::Result<()> {
        while !self.buffer.is_empty() {
            let (to_send, _) = self.buffer.as_slices();
            let count = self.writer.write(to_send).await?;
            self.buffer.drain(..count);
        }
        Ok(())
    }

    pub async fn close(&mut self) -> std::io::Result<()> {
        self.flush().await
        // TODO: Finish the stream
    }
}

impl<'p, Sample> futures::sink::Sink<Sample> for AlsaBufferedWriter<'p, Sample>
where
    Sample: alsa::pcm::IoFormat + Unpin,
{
    type Error = std::io::Error;

    fn poll_ready(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let p = std::pin::pin!(self.ready());
        p.poll(cx)
    }

    fn start_send(self: std::pin::Pin<&mut Self>, item: Sample) -> Result<(), Self::Error> {
        std::pin::pin!(self).send(item)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let p = std::pin::pin!(self.flush());
        p.poll(cx)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }
}

#[tokio::main]
async fn main() {
    const DEVICE_NAME: &str = "default";
    let mut phase: f32 = 0.0;

    let alsa = AlsaPlayback::new(DEVICE_NAME);

    let mut data = [0.0; 65536];

    let writer = AlsaWriter::new(&alsa);

    let use_sink = false;

    if use_sink {
        let mut sink = AlsaBufferedWriter::new(writer);

        loop {
            generate_data(&mut data, alsa.get_rate(), &mut phase);
            println!("phase={phase}");

            for i in data {
                use futures::sink::SinkExt as _;
                sink.feed(i).await.expect("Failed to sink sample");
            }
        }
    } else {
        let mut buffered = AlsaBufferedWriter::new(writer);
        loop {
            generate_data(&mut data, alsa.get_rate(), &mut phase);
            println!("phase={phase}");

            for i in data {
                buffered.ready().await.expect("Failed to become ready");
                buffered.send(i).expect("Failed to send sample");
            }
        }
    }
}
