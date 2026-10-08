#pragma once
#include <stdint.h>

/* ABI 2: one serialized, non-shared direction per handle. No callbacks or borrowed buffers.
 * PCM: 48000 Hz, mono signed16, 960 samples/frame. RTP: EVS Primary CBR7.2-128k,
 * NB/WB/SWB/FB, 20ms. One or two independent coded channels; stereo encoding duplicates
 * the mono source, decoding mixes channels to mono. Stereo requires Header-Full-only.
 * Receive packets contain up to six complete frame-blocks (120ms).
 * Create returns0 or a negative error; encode/decode return length or a negative error.
 * Wrong input leaves output untouched. An internal codec error makes the handle unusable.
 * Destroy accepts NULL. A destroyed or arbitrary pointer must never be supplied again. */
#if defined(_WIN32) && defined(PHONE_EVS_BUILD)
#define API __declspec(dllexport)
#elif defined(_WIN32)
#define API __declspec(dllimport)
#else
#define API __attribute__((visibility("default")))
#endif

API int ivy_phone_evs_abi(void);
API int ivy_phone_evs_create(int encoding, int bitrate, int bandwidth, int channels, void **handle);
API void ivy_phone_evs_destroy(void *handle);
API int ivy_phone_evs_encode(void *handle, const int16_t *pcm, int samples,
    uint8_t *payload, int capacity, int header_full);
API int ivy_phone_evs_decode(void *handle, const uint8_t *payload, int bytes,
    int16_t *pcm, int capacity, int header_full_only);
