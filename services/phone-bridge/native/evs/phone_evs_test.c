/* Deterministic synthetic codec checks; no audio device, SIP, Desktop or network. */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <math.h>
#include <stdlib.h>
#include "phone_evs.h"

static void check(int valid, const char *message) {
    if (!valid) { fprintf(stderr, "phone_evs_failed: %s\n", message); exit(1); }
}
static const int rates[] = {7200, 8000, 9600, 13200, 16400, 24400, 32000, 48000, 64000, 96000, 128000};

static void roundtrip(int rate_index, int bandwidth, int channels, int full) {
    int rate = rates[rate_index], frame_bytes = rate / 400, headers = full ? channels : 0;
    int bytes = channels * frame_bytes + headers;
    void *encoder = NULL, *decoder = NULL;
    check(!ivy_phone_evs_create(1, rate, bandwidth, channels, &encoder) &&
        !ivy_phone_evs_create(0, rate, bandwidth, channels, &decoder), "separate streaming directions");
    int16_t pcm[960], decoded[5761]; uint8_t packet[643], saved[643], recent[6][643]; double energy = 0;
    for (int frame = 0; frame < 30; frame++) {
        for (int i = 0; i < 960; i++) pcm[i] = (int16_t)(8000 * sin((frame * 960 + i) * 6.283185307179586 * 440 / 48000));
        memset(packet, 0x55, sizeof(packet));
        check(ivy_phone_evs_encode(encoder, pcm, 959, packet, bytes, full) < 0 && packet[0] == 0x55, "wrong frame cannot encode");
        check(ivy_phone_evs_encode(encoder, pcm, 960, packet, bytes - 1, full) < 0 && packet[0] == 0x55, "short packet capacity cannot encode");
        check(ivy_phone_evs_encode(encoder, pcm, 960, packet, bytes, full) == bytes && packet[bytes] == 0x55,
            "exact negotiated bitrate and channels with intact output guard");
        if (full) for (int channel = 0; channel < channels; channel++)
            check(packet[channel] == (uint8_t)((rate_index + 1) | (channel + 1 < channels ? 0x40 : 0)), "correct ToCs for every coded channel");
        memcpy(saved, packet, sizeof(packet)); decoded[960] = 12345;
        check(ivy_phone_evs_decode(decoder, packet, bytes, decoded, 959, full) < 0, "short output rejected before decoding");
        check(ivy_phone_evs_decode(decoder, packet, bytes, decoded, 960, full) == 960 && decoded[960] == 12345,
            "one 20ms decoded mono frame with intact output guard");
        check(!memcmp(saved, packet, sizeof(packet)), "RTP input is immutable");
        if (frame >= 10) for (int i = 0; i < 960; i++) energy += (double)decoded[i] * decoded[i];
        if (frame >= 24) memcpy(recent[frame - 24], packet, sizeof(packet));
    }
    check(energy / (20 * 960) > 1000000, "real decoded tone after codec delay");
    // Every supported 20..120ms receive duration, including stereo frame-block ordering.
    uint8_t multiple[4096];
    for (int blocks = 1; blocks <= 6; blocks++) {
        int frames = blocks * channels, length = frames * (frame_bytes + 1);
        for (int frame = 0; frame < frames; frame++) {
            multiple[frame] = (uint8_t)((rate_index + 1) | (frame + 1 < frames ? 0x40 : 0));
            memcpy(multiple + frames + frame * frame_bytes, recent[frame / channels] + headers + frame % channels * frame_bytes, (size_t)frame_bytes);
        }
        decoded[blocks * 960] = 12345;
        check(ivy_phone_evs_decode(decoder, multiple, length, decoded, blocks * 960, 1) == blocks * 960 && decoded[blocks * 960] == 12345,
            "all complete frame-blocks respect the 120ms limit and mono output bounds");
        if (blocks == 6) {
            multiple[frames - 1] |= 0x40; multiple[length] = 15;
            check(ivy_phone_evs_decode(decoder, multiple, length + 1, decoded, 5760, 1) < 0, "seventh frame-block rejected");
        }
    }
    for (int i = 0; i < 5761; i++) decoded[i] = 12345;
    uint8_t malformed[] = {0x46, 0x0d};
    check(ivy_phone_evs_decode(decoder, malformed, sizeof(malformed), decoded, 5760, 1) < 0, "malformed packet rejected");
    for (int i = 0; i < 5761; i++) check(decoded[i] == 12345, "invalid packet never returns partial PCM");
    check(ivy_phone_evs_decode(decoder, packet, bytes, decoded, 960, full) == 960, "malformed packet does not poison state");
    check(ivy_phone_evs_decode(decoder, packet, 4097, decoded, 5760, full) < 0, "oversized input rejected before access");
    check(ivy_phone_evs_decode(encoder, packet, bytes, decoded, 960, full) < 0 &&
        ivy_phone_evs_encode(decoder, pcm, 960, packet, bytes, full) < 0, "directions cannot be exchanged");
    if (channels == 2) {
        check(ivy_phone_evs_decode(decoder, packet + 1, 1 + frame_bytes, decoded, 960, 1) < 0, "incomplete stereo block rejected");
        check(ivy_phone_evs_encode(encoder, pcm, 960, packet, bytes, 0) < 0, "stereo cannot use Compact format");
    }
    ivy_phone_evs_destroy(encoder); ivy_phone_evs_destroy(decoder);
}

static void stereo_mix(void) {
    void *enc[2] = {NULL, NULL}, *mono[2] = {NULL, NULL}, *stereo = NULL;
    for (int ch = 0; ch < 2; ch++) check(!ivy_phone_evs_create(1, 128000, 3, 1, &enc[ch]) &&
        !ivy_phone_evs_create(0, 128000, 3, 1, &mono[ch]), "independent mono fixtures");
    check(!ivy_phone_evs_create(0, 128000, 3, 2, &stereo), "independent stereo decoder");
    int16_t pcm[960], individual[2][960], mixed[960]; uint8_t packet[642]; packet[0] = 0x4b; packet[1] = 0x0b;
    for (int frame = 0; frame < 30; frame++) {
        for (int ch = 0; ch < 2; ch++) {
            for (int i = 0; i < 960; i++) pcm[i] = (int16_t)(8000 * sin((frame * 960 + i) * 6.283185307179586 * (ch ? 997 : 440) / 48000));
            check(ivy_phone_evs_encode(enc[ch], pcm, 960, packet + 2 + ch * 320, 320, 0) == 320, "two different coded channel signals");
            check(ivy_phone_evs_decode(mono[ch], packet + 2 + ch * 320, 320, individual[ch], 960, 0) == 960, "independent expected signal");
        }
        check(ivy_phone_evs_decode(stereo, packet, sizeof(packet), mixed, 960, 1) == 960, "stereo frame-block decoded once");
        for (int i = 0; i < 960; i++) check(abs((int)mixed[i] - ((int)individual[0][i] + individual[1][i]) / 2) <= 2,
            "stereo downmix preserves both independent channel histories without doubling duration or gain");
    }
    for (int ch = 0; ch < 2; ch++) { ivy_phone_evs_destroy(enc[ch]); ivy_phone_evs_destroy(mono[ch]); }
    ivy_phone_evs_destroy(stereo);
}

int main(void) {
    check(ivy_phone_evs_abi() == 2, "exact ABI");
    void *invalid = NULL;
    check(ivy_phone_evs_create(2, 128000, 3, 1, &invalid) < 0 && invalid == NULL, "invalid direction has no handle");
    check(ivy_phone_evs_create(1, 128000, 0, 1, &invalid) < 0 && invalid == NULL, "unsupported NB bitrate rejected");
    check(ivy_phone_evs_create(1, 7200, 3, 1, &invalid) < 0 && invalid == NULL, "unsupported FB bitrate rejected");
    check(ivy_phone_evs_create(1, 5900, 1, 1, &invalid) < 0 && invalid == NULL, "SC-VBR without DTX is not advertised");
    check(ivy_phone_evs_create(1, 128000, 3, 3, &invalid) < 0 && invalid == NULL, "unadvertised channels rejected");
    ivy_phone_evs_destroy(NULL);
    int profiles = 0;
    for (int rate = 0; rate < 11; rate++) for (int bw = 0; bw < 4; bw++) {
        if ((bw == 0 && rates[rate] > 24400) || (bw == 2 && rates[rate] < 9600) || (bw == 3 && rates[rate] < 16400)) continue;
        roundtrip(rate, bw, 1, 0); roundtrip(rate, bw, 1, 1); roundtrip(rate, bw, 2, 1); profiles++;
    }
    stereo_mix();
    printf("phone_evs_passed: %d bitrate/bandwidth profiles, Compact/Header-Full, mono/stereo, 20..120ms, PCM, bounds and direction isolation\n", profiles);
    return 0;
}
