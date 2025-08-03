fn generate_data(buffer: &mut [f32], rate: u32, phase: &mut f32) {
    const FREQUENCY: f32 = 440.0;
    for i in buffer {
        *i = (*phase * std::f32::consts::TAU * FREQUENCY / (rate as f32)).sin();
        *phase += 1.0;
    }
}

fn main() {
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

    let mut fds = alsa::poll::Descriptors::get(&pcm).expect("Couldn't get ALSA PCM FDs");

    for fd in &fds {
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
    }

    let (buffer_size, period_size) = pcm.get_params().unwrap();

    println!("{buffer_size} {period_size}");

    let mut data = [0.0; 65536];
    let mut to_send = &data[..0];

    let mut output = alsa::Output::buffer_open().expect("couldn't open output");
    pcm.dump(&mut output).expect("dump failed");
    println!("{output}");

    loop {
        let mut max_fd = -1;
        if to_send.is_empty() {
            generate_data(&mut data, rate, &mut phase);
            to_send = &data;
        }
        unsafe {
            let mut rfd: libc::fd_set = std::mem::zeroed();
            let mut wfd: libc::fd_set = std::mem::zeroed();
            let mut xfd: libc::fd_set = std::mem::zeroed();
            libc::FD_ZERO(&mut rfd as *mut _);
            libc::FD_ZERO(&mut wfd as *mut _);
            libc::FD_ZERO(&mut xfd as *mut _);
            for fd in &fds {
                if fd.events & libc::POLLIN != 0 {
                    libc::FD_SET(fd.fd, &mut rfd as *mut _)
                }
                if fd.events & libc::POLLOUT != 0 {
                    libc::FD_SET(fd.fd, &mut wfd as *mut _)
                }
                if fd.events & libc::POLLERR != 0 {
                    libc::FD_SET(fd.fd, &mut xfd as *mut _)
                }
                if fd.events != 0 && fd.fd > max_fd {
                    max_fd = fd.fd;
                }
            }

            let ret = libc::select(
                max_fd + 1,
                &mut rfd,
                &mut wfd,
                &mut xfd,
                std::ptr::null_mut(),
            );
            if ret == -1 {
                panic!("select");
            }

            for fd in &mut fds {
                fd.revents = if libc::FD_ISSET(fd.fd, &mut rfd as *mut _) {
                    libc::POLLIN
                } else {
                    0
                } | if libc::FD_ISSET(fd.fd, &mut wfd as *mut _) {
                    libc::POLLOUT
                } else {
                    0
                } | if libc::FD_ISSET(fd.fd, &mut xfd as *mut _) {
                    libc::POLLERR
                } else {
                    0
                };
            }
            let flags = alsa::poll::Descriptors::revents(&pcm, &fds).expect("Failed to remap");
            println!("{flags:?}");

            if flags.contains(alsa::poll::Flags::OUT) {
                pcm.avail_update().expect("Failed to update avail");
                let frames = pcm.avail().unwrap();
                let io = pcm.io_f32().unwrap();

                let count = io
                    .writei(&to_send[..std::cmp::min(frames as usize, to_send.len())])
                    .expect("write failed");
                to_send = &to_send[count..];
                println!("{count}");
            }
        }
    }
}
