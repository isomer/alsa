#include <alsa/asoundlib.h>
#include <stdint.h>
#include <err.h>
#include <stdio.h>
#include <math.h>

#define ALSA_CHECK(x) if ( (errval = (x)) < 0 ) errx(1, #x ": %s", snd_strerror(errval))

static void generate_data(float *buffer, size_t buffer_size, unsigned int rate) {
    static float phase = 0.0f;
    const float frequency = 440.0;

    for(size_t idx = 0; idx < buffer_size; ++idx) {
        buffer[idx] = sin(phase * 2.0 * M_PI * frequency / rate);
        phase += 1.0;
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

    printf("Rate: %u\n", rate);
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

    snd_pcm_uframes_t buffer_size;
    snd_pcm_uframes_t period_size;
    ALSA_CHECK(snd_pcm_get_params(pcm_handle, &buffer_size, &period_size));

    printf("%lu %lu\n", buffer_size, period_size);

    snd_output_t *output;
    ALSA_CHECK(snd_output_stdio_attach(&output, stdout, 0));
    ALSA_CHECK(snd_pcm_dump(pcm_handle, output));


    const size_t data_size = 65536;
    float data[data_size];
    size_t data_len = 0;
    float *data_ptr = NULL;


    for(;;) {
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
        if (ret == -1)
            err(1, "select");

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

        printf("%s%s%s\n",
                revents & POLLIN ? " POLLIN" : "",
                revents & POLLOUT ? " POLLOUT" : "",
                revents & POLLERR ? " POLLERR" : "");

        if (revents & POLLOUT) {
            if (data_len == 0) {
                data_ptr = data;
                generate_data(data_ptr, data_size, rate);
                data_len = data_size;
            }

            ret = snd_pcm_writei(pcm_handle, data, data_len /* in frames, not samples */);
            if (ret < 0) errx(1, "snd_pcm_writei: %s", snd_strerror(ret));
            printf("%d\n", ret);

            data_ptr += ret;
            data_len -= ret;
        }

    }

    return 0;
}

