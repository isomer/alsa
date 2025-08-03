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

#[tokio::main]
async fn main() {
    const DEVICE_NAME: &str = "default";
    let mut phase: f32 = 0.0;

    let pcm = alsa::PCM::new(DEVICE_NAME, alsa::Direction::Playback, true)
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

    let fds = alsa::poll::Descriptors::get(&pcm).expect("Couldn't get ALSA PCM FDs");
    let fd = fds.first().unwrap();
    let async_fd = tokio::io::unix::AsyncFd::new(fd.fd).expect("couldn't get async fd");

    println!(
        "fd{}{}{}{}",
        fd.fd,
        if fd.events & libc::POLLIN != 0 {
            " POLLIN"
        } else {
            ""
        },
        if fd.events & libc::POLLOUT != 0 {
            " POLLOUT"
        } else {
            ""
        },
        if fd.events & libc::POLLERR != 0 {
            " POLLERR"
        } else {
            ""
        },
    );

    let (buffer_size, period_size) = pcm.get_params().unwrap();

    println!("buffer_size={buffer_size} period_size={period_size}");

    let mut data = [0.0; 65536];
    let mut to_send = &data[..0];

    let mut output = alsa::Output::buffer_open().expect("couldn't open output");
    pcm.dump(&mut output).expect("dump failed");
    println!("{output}");

    loop {
        if to_send.is_empty() {
            generate_data(&mut data, rate, &mut phase);
            to_send = &data;
        }
        // Even for write only use like this, alsa often requires read events, since it's asking
        // you to wait on a status pipe rather than the underlying audio device.
        let interest = if fd.events & libc::POLLIN != 0 {
            tokio::io::Interest::READABLE
        } else if fd.events & libc::POLLOUT != 0 {
            tokio::io::Interest::WRITABLE
        } else {
            panic!("unexpected interest");
        };
        let mut guard = async_fd
            .ready(interest)
            .await
            .expect("Failed to get asyncfd guard");

        let io_result = guard.try_io(|_fd| {
            // As this is an example program for async i/o only, we are not handling XRUN or other
            // failures, just aborting to keep the code clear to understand the primary point.

            //let current_state = pcm.state();
            //assert_eq!(current_state, alsa::pcm::State::Running);

            let fds = [libc::pollfd {
                fd: fd.fd,
                events: fd.events,
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
                alsa::poll::Descriptors::revents(&pcm, &fds).expect("Failed to alsa revents");

            pcm.avail_update().expect("Failed to update ALSA avail");

            println!("flags={flags:?}");
            if flags.contains(alsa::poll::Flags::OUT) {
                let frames = pcm.avail().unwrap();
                let io = pcm.io_f32().unwrap();
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
            Ok(Ok(())) => (),
            Ok(Err(err)) => panic!("error: {err:?}"),
            Err(_would_block) => {}
        }
    }
}
