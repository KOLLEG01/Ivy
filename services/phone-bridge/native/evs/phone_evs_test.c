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

static void roundtrip(int full) {
    void *encoder = NULL, *decoder = NULL;
    check(!ivy_phone_evs_create(1, &encoder) && !ivy_phone_evs_create(0, &decoder), "separate streaming directions");
    int16_t pcm[640], decoded[3841]; uint8_t packet[62], saved[62]; double energy = 0;
    for (int frame = 0; frame < 50; frame++) {
        for (int i = 0; i < 640; i++) pcm[i] = (int16_t)(8000 * sin((frame * 640 + i) * 6.283185307179586 * 440 / 32000));
        memset(packet, 0x55, sizeof(packet));
        check(ivy_phone_evs_encode(encoder, pcm, 639, packet, 62, full) < 0 && packet[0] == 0x55, "wrong frame cannot encode");
        int bytes = ivy_phone_evs_encode(encoder, pcm, 640, packet, 62, full);
        check(bytes == 61 + full && (!full || packet[0] == 6), "exact 24.4k compact/header-full payload");
        memcpy(saved, packet, sizeof(packet)); decoded[640] = 12345;
        check(ivy_phone_evs_decode(decoder, packet, bytes, decoded, 639, full) < 0, "short output rejected before decoding");
        check(ivy_phone_evs_decode(decoder, packet, bytes, decoded, 640, full) == 640 && decoded[640] == 12345,
            "one 20ms decoded frame with intact output guard");
        check(!memcmp(saved, packet, sizeof(packet)), "RTP input is immutable");
        if (frame > 10) for (int i = 0; i < 640; i++) energy += (double)decoded[i] * decoded[i];
    }
    check(energy / (39 * 640) > 1000000, "real decoded tone after codec delay");
    uint8_t multiple[6 * 62];
    for (int i = 0; i < 6; i++) {
        multiple[i] = (uint8_t)(6 | (i < 5 ? 0x40 : 0));
        memcpy(multiple + 6 + i * 61, packet + full, 61);
    }
    decoded[3840] = 12345;
    check(ivy_phone_evs_decode(decoder, multiple, sizeof(multiple), decoded, 3840, 1) == 3840 && decoded[3840] == 12345,
        "six header-full frames respect the 120ms limit");
    for (int i = 0; i < 3841; i++) decoded[i] = 12345;
    uint8_t malformed[] = {0x46, 0x0d}; // Later reserved ToC, no valid speech data.
    check(ivy_phone_evs_decode(decoder, malformed, sizeof(malformed), decoded, 3840, 1) < 0, "malformed packet rejected");
    for (int i = 0; i < 3841; i++) check(decoded[i] == 12345, "invalid packet never returns partial PCM");
    check(ivy_phone_evs_decode(decoder, packet, 4097, decoded, 3840, full) < 0, "oversized input rejected before access");
    check(ivy_phone_evs_decode(encoder, packet, 61 + full, decoded, 640, full) < 0 &&
        ivy_phone_evs_encode(decoder, pcm, 640, packet, 62, full) < 0, "directions cannot be exchanged");
    ivy_phone_evs_destroy(encoder); ivy_phone_evs_destroy(decoder);
    printf("phone_evs_roundtrip: headerFull=%d frames=50 rms=%.2f\n", full, sqrt(energy / (39 * 640)));
}

int main(void) {
    check(ivy_phone_evs_abi() == 1, "exact ABI");
    void *invalid = NULL;
    check(ivy_phone_evs_create(2, &invalid) < 0 && invalid == NULL, "invalid direction has no handle");
    ivy_phone_evs_destroy(NULL);
    roundtrip(0); roundtrip(1);
    puts("phone_evs_passed: compact/header-full, actual PCM, six frames, bounds and direction isolation");
    return 0;
}
