Experimentation with async alsa playback.

if use\_sink is false, then everything works as expected.

If use\_sink is true, then the program blocks forever as soon as the buffer is full.

the difference is just a minor wrapper change.
