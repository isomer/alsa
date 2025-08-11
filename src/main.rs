fn generate_data(buffer: &mut [f32], rate: u32, phase: &mut f32) {
    const FREQUENCY: f32 = 440.0;
    for i in buffer {
        *i = (*phase * std::f32::consts::TAU * FREQUENCY / (rate as f32)).sin();
        *phase += 1.0;
    }
    if *phase > rate as f32 {
        *phase -= rate as f32;
    }
}

struct AlsaPlayback {
    pcm: alsa::PCM,
    async_fd: tokio::io::unix::AsyncFd<std::os::fd::RawFd>,
    poll_fd: libc::pollfd,
    rate: f32,
}

impl AlsaPlayback {
    fn new(device: &str) -> Self {
        let pcm = alsa::PCM::new(device, alsa::Direction::Playback, true)
            .expect("Failed to open device for playback");

        let hwparams = alsa::pcm::HwParams::any(&pcm).unwrap();
        hwparams
            .set_access(alsa::pcm::Access::RWInterleaved)
            .unwrap();
        hwparams.set_format(alsa::pcm::Format::FloatLE).unwrap();
        let rate = hwparams
            .set_rate_near(44100, alsa::ValueOr::Nearest)
            .unwrap();

        println!("Rate: {rate}");
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

    async fn write(&self, mut to_send: &[f32]) -> std::io::Result<()> {
        let interest = self.get_interest();
        let mut guard = self
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
                fd: self.poll_fd.fd,
                events: self.poll_fd.events,
                revents: match interest {
                    tokio::io::Interest::READABLE => libc::POLLIN,
                    tokio::io::Interest::WRITABLE => libc::POLLOUT,
                    _ => unimplemented!(),
                },
            }];

            // Since ALSA may have asked for a POLLIN event for us to write (since it's actually
            // waiting on a status pipe), we need to remap that back to OUT, some alsa plugins
            // rely on this to perform some internal book keeping updates.  This does that.
            let flags =
                alsa::poll::Descriptors::revents(&self.pcm, &fds).expect("Failed to alsa revents");

            self.pcm
                .avail_update()
                .expect("Failed to update ALSA avail");

            println!("flags={flags:?}");
            if flags.contains(alsa::poll::Flags::OUT) {
                let frames = self.pcm.avail().unwrap();
                let io = self.pcm.io_f32().unwrap();
                let count = io
                    .writei(&to_send[..std::cmp::min(frames as usize, to_send.len())])
                    .expect("write failed");
                to_send = &to_send[count..];
                println!("{count}");
            } else {
                // ALSA is NOT ready for writing according to its internal logic (alsa_flags).
                // Return WouldBlock to prevent the spin: this tells Tokio to re-poll the FD.
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "ALSA not ready for write according to its revents flags",
                ));
            }

            Ok(())
        });

        match io_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(err),
            Err(_would_block) => Ok(()),
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

#[tokio::main]
async fn main() {
    const DEVICE_NAME: &str = "default";
    let mut phase: f32 = 0.0;

    let alsa = AlsaPlayback::new(DEVICE_NAME);

    let mut data = [0.0; 65536];
    let mut to_send = &data[..0];

    println!("{alsa:?}");

    loop {
        if to_send.is_empty() {
            generate_data(&mut data, alsa.get_rate() as u32, &mut phase);
            to_send = &data;
        }

        alsa.write(to_send).await.expect("Unexpected error");
    }
}
