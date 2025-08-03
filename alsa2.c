#include <alsa/asoundlib.h>
#include <stdint.h>
#include <err.h>
#include <stdio.h>
#include <math.h>
#include <poll.h> // For pollfd and POLLIN/POLLOUT

#define ALSA_CHECK(x) if ( (errval = (x)) < 0 ) errx(1, #x ": %s", snd_strerror(errval))

static void generate_data(float *buffer, size_t buffer_size, unsigned int rate) {
    static float phase = 0.0f;
    const float frequency = 440.0; // A4 note

    for(size_t idx = 0; idx < buffer_size; ++idx) {
        // Generate a sine wave sample
        buffer[idx] = sin(phase * 2.0 * M_PI * frequency / rate);
        phase += 1.0;
        // Reset phase to prevent overflow for long running applications
        if (phase >= rate) {
            phase -= rate;
        }
    }
}

int main(int argc, char *argv[]) {
    (void) argc;
    (void) argv;

    int errval;
    const char* device_name = "default";
    snd_pcm_t *pcm_handle;
    unsigned int rate = 44100;

    ALSA_CHECK(snd_pcm_open(&pcm_handle, device_name, SND_PCM_STREAM_PLAYBACK, SND_PCM_ASYNC));

    uint8_t hw_params_raw_data[snd_pcm_hw_params_sizeof()];
    snd_pcm_hw_params_t *hwparams = (snd_pcm_hw_params_t *)hw_params_raw_data;

    ALSA_CHECK(snd_pcm_hw_params_any(pcm_handle, hwparams));
    ALSA_CHECK(snd_pcm_hw_params_set_access(pcm_handle, hwparams, SND_PCM_ACCESS_RW_INTERLEAVED));
    ALSA_CHECK(snd_pcm_hw_params_set_format(pcm_handle, hwparams, SND_PCM_FORMAT_FLOAT_LE));
    ALSA_CHECK(snd_pcm_hw_params_set_rate_near(pcm_handle, hwparams, &rate, NULL));

    printf("Actual Rate: %u\n", rate);
    ALSA_CHECK(snd_pcm_hw_params_set_channels(pcm_handle, hwparams, 1)); /* Mono */

    ALSA_CHECK(snd_pcm_hw_params(pcm_handle, hwparams));
    size_t fd_count = snd_pcm_poll_descriptors_count(pcm_handle);
    struct pollfd fds[fd_count];

    ALSA_CHECK(snd_pcm_poll_descriptors(pcm_handle, fds, fd_count));

    for(size_t i = 0; i < fd_count; ++i) {
        printf("%zd: fd%d%s%s%s\n",
                i,
                fds[i].fd,
                fds[i].events & POLLIN ? " POLLIN" : "",
                fds[i].events & POLLOUT ? " POLLOUT" : "",
                fds[i].events & POLLERR ? " POLLERR" : "");
    }

    snd_pcm_uframes_t buffer_size_frames;
    snd_pcm_uframes_t period_size_frames;
    ALSA_CHECK(snd_pcm_get_params(pcm_handle, &buffer_size_frames, &period_size_frames));

    printf("Buffer size (frames): %lu, Period size (frames): %lu\n", buffer_size_frames, period_size_frames);

    snd_output_t *output;
    ALSA_CHECK(snd_output_stdio_attach(&output, stdout, 0));
    ALSA_CHECK(snd_pcm_dump(pcm_handle, output));


    const size_t local_data_buffer_size = 65536; // A reasonable buffer size for local data (can be anything)
    float local_data_buffer[local_data_buffer_size];
    ssize_t frames_to_write_from_local_buffer = 0; // How many frames are currently in our local buffer
    float *local_data_ptr = NULL;


    for(;;) {
        // If our local buffer is empty or completely written, generate more data
        if (frames_to_write_from_local_buffer == 0) {
            generate_data(local_data_buffer, local_data_buffer_size, rate);
            local_data_ptr = local_data_buffer;
            frames_to_write_from_local_buffer = local_data_buffer_size;
            printf("Generated new data block (%zu frames)\n", local_data_buffer_size);
        }

        fd_set rfd;
        fd_set wfd;
        fd_set xfd;
        FD_ZERO(&rfd);
        FD_ZERO(&wfd);
        FD_ZERO(&xfd);
        int max_fd = -1;
        for(size_t i = 0; i < fd_count; ++i) {
            if (fds[i].events & POLLIN) FD_SET(fds[i].fd, &rfd);
            if (fds[i].events & POLLOUT) FD_SET(fds[i].fd, &wfd);
            if (fds[i].events & POLLERR) FD_SET(fds[i].fd, &xfd);
            if (fds[i].events && fds[i].fd > max_fd)
                max_fd = fds[i].fd;
        }

        int ret = select(max_fd + 1, &rfd, &wfd, &xfd, NULL);
        if (ret == -1) {
            // Check for EINTR, which can happen if a signal is caught
            if (errno == EINTR) {
                continue; // Retry select
            }
            err(1, "select");
        }

        for(size_t i = 0; i < fd_count; ++i) {
            fds[i].revents =
                (FD_ISSET(fds[i].fd, &rfd) ? POLLIN : 0) |
                (FD_ISSET(fds[i].fd, &wfd) ? POLLOUT : 0) |
                (FD_ISSET(fds[i].fd, &xfd) ? POLLERR : 0);
        }

        unsigned short revents = 0;
        ALSA_CHECK(snd_pcm_poll_descriptors_revents(
                    pcm_handle,
                    fds,
                    fd_count,
                    &revents));

        printf("Poll events: %s%s%s\n",
                   revents & POLLIN ? " POLLIN" : "",
                   revents & POLLOUT ? " POLLOUT" : "",
                   revents & POLLERR ? " POLLERR" : "");

        if (revents & POLLOUT) {
            snd_pcm_sframes_t frames_available;
            snd_pcm_state_t state = snd_pcm_state(pcm_handle);

            if (state == SND_PCM_STATE_XRUN) {
                // Handle underrun/overrun
                ret = snd_pcm_prepare(pcm_handle);
                if (ret < 0) errx(1, "snd_pcm_prepare after XRUN: %s", snd_strerror(ret));
                printf("ALSA underrun, attempting to recover.\n");
                continue; // Try again after preparing
            } else if (state == SND_PCM_STATE_SUSPENDED) {
                ret = snd_pcm_resume(pcm_handle);
                if (ret < 0 || ret == 0) { // 0 means it's not possible to resume
                    ret = snd_pcm_prepare(pcm_handle);
                    if (ret < 0) errx(1, "snd_pcm_prepare after SUSPENDED: %s", snd_strerror(ret));
                }
                printf("ALSA suspended, attempting to resume/prepare.\n");
                continue; // Try again
            }

            // Get the number of frames that can be written without blocking
            frames_available = snd_pcm_avail_update(pcm_handle);
            if (frames_available < 0) {
                if (frames_available == -EPIPE) { // XRUN (underrun/overrun)
                    ret = snd_pcm_prepare(pcm_handle);
                    if (ret < 0) errx(1, "snd_pcm_prepare after avail_update XRUN: %s", snd_strerror(ret));
                    printf("ALSA underrun detected by avail_update, attempting to recover.\n");
                    continue;
                } else {
                    errx(1, "snd_pcm_avail_update: %s", snd_strerror(frames_available));
                }
            }

            // Determine how many frames to write
            snd_pcm_sframes_t frames_to_write_this_iter = frames_available;
            if (frames_to_write_this_iter > frames_to_write_from_local_buffer) {
                frames_to_write_this_iter = frames_to_write_from_local_buffer;
            }

            if (frames_to_write_this_iter > 0) {
                ret = snd_pcm_writei(pcm_handle, local_data_ptr, frames_to_write_this_iter);
                printf("writei %i\n", ret);
                if (ret < 0) {
                    if (ret == -EPIPE) { // XRUN (underrun/overrun)
                        ret = snd_pcm_prepare(pcm_handle);
                        if (ret < 0) errx(1, "snd_pcm_prepare after writei XRUN: %s", snd_strerror(ret));
                        printf("ALSA underrun detected by writei, attempting to recover.\n");
                        continue;
                    } else if (ret == -ESTRPIPE) { // Suspended
                        ret = snd_pcm_resume(pcm_handle);
                        if (ret < 0 || ret == 0) {
                            ret = snd_pcm_prepare(pcm_handle);
                            if (ret < 0) errx(1, "snd_pcm_prepare after writei SUSPENDED: %s", snd_strerror(ret));
                        }
                        printf("ALSA suspended detected by writei, attempting to resume/prepare.\n");
                        continue;
                    } else {
                        errx(1, "snd_pcm_writei: %s", snd_strerror(ret));
                    }
                }
                // printf("Wrote %d frames\n", ret);

                local_data_ptr += ret;
                frames_to_write_from_local_buffer -= ret;
            }
        }
    }

    // Cleanup (though this loop runs indefinitely)
    snd_output_close(output);
    snd_pcm_close(pcm_handle);

    return 0;
}
