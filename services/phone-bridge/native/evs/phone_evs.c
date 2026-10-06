/* Ivy Phone streaming ABI around the separately obtained ETSI TS 126 443 reference.
 * One managed direction owns this handle and serializes every call, including destroy.
 * Reference implementation and RTP parser remain external pinned build inputs. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "cnst.h"
#include "prot.h"
#include "evs_rtp_payload.h"
#define PHONE_EVS_BUILD
#include "phone_evs.h"

enum { FRAME_SAMPLES = 640, FRAME_BYTES = 61, MAX_FRAMES = 6, MAX_PAYLOAD = 4096 };
enum { BAD_ARGUMENT = -1, NO_MEMORY = -2, BAD_PAYLOAD = -3, BAD_STATE = -4 };

typedef struct {
    Encoder_State *encoder;
    Decoder_State *decoder;
    Indice *indices;
    int ready, failed;
} PhoneEvs;

API int ivy_phone_evs_abi(void) { return 1; }

API void ivy_phone_evs_destroy(void *handle) {
    PhoneEvs *codec = (PhoneEvs *)handle;
    if (!codec) return;
    if (codec->ready) {
        if (codec->encoder) destroy_encoder(codec->encoder);
        if (codec->decoder) destroy_decoder(codec->decoder);
    }
    if (codec->encoder) { memset(codec->encoder, 0, sizeof(*codec->encoder)); free(codec->encoder); }
    if (codec->decoder) { memset(codec->decoder, 0, sizeof(*codec->decoder)); free(codec->decoder); }
    if (codec->indices) { memset(codec->indices, 0, MAX_NUM_INDICES * sizeof(Indice)); free(codec->indices); }
    memset(codec, 0, sizeof(*codec)); free(codec);
}

API int ivy_phone_evs_create(int encoding, void **handle) {
    if (!handle || (encoding != 0 && encoding != 1)) return BAD_ARGUMENT;
    *handle = NULL;
    PhoneEvs *codec = (PhoneEvs *)calloc(1, sizeof(*codec));
    if (!codec) return NO_MEMORY;
    if (encoding) {
        codec->encoder = (Encoder_State *)calloc(1, sizeof(Encoder_State));
        codec->indices = (Indice *)calloc(MAX_NUM_INDICES, sizeof(Indice));
        if (!codec->encoder || !codec->indices) { ivy_phone_evs_destroy(codec); return NO_MEMORY; }
        Encoder_State *state = codec->encoder;
        state->input_Fs = 32000; state->total_brate = 24400; state->max_bwidth = SWB;
        state->Opt_AMR_WB = 0; state->Opt_DTX_ON = 0; state->Opt_RF_ON = 0;
        state->rf_fec_offset = 0; state->rf_fec_indicator = 1;
        state->interval_SID = FIXED_SID_RATE; state->var_SID_rate_flag = 1;
        state->Opt_SC_VBR = 0; state->last_Opt_SC_VBR = 0;
        state->bitstreamformat = MIME; state->codec_mode = MODE2; state->last_codec_mode = MODE2;
        state->ind_list = codec->indices;
        init_encoder(state);
    } else {
        codec->decoder = (Decoder_State *)calloc(1, sizeof(Decoder_State));
        if (!codec->decoder) { ivy_phone_evs_destroy(codec); return NO_MEMORY; }
        Decoder_State *state = codec->decoder;
        state->output_Fs = 32000; state->codec_mode = 0; state->Opt_AMR_WB = 0;
        state->Opt_VOIP = 1; state->bitstreamformat = VOIP_RTPDUMP;
        state->sdp_hf_only = 0; state->amrwb_rfc4867_flag = -1;
        state->ini_frame = 0; state->prev_use_partial_copy = 0;
        init_decoder(state); reset_indices_dec(state);
    }
    codec->ready = 1; *handle = codec; return 0;
}

/* Positive result is the exact payload length; errors never expose partial output. */
API int ivy_phone_evs_encode(void *handle, const int16_t *pcm, int samples,
    uint8_t *payload, int capacity, int header_full) {
    PhoneEvs *codec = (PhoneEvs *)handle;
    if (!codec || !codec->ready || codec->failed || !codec->encoder) return BAD_STATE;
    if (!pcm || !payload || samples != FRAME_SAMPLES || (header_full != 0 && header_full != 1) ||
        capacity < FRAME_BYTES + header_full) return BAD_ARGUMENT;
    short input[FRAME_SAMPLES]; UWord8 bits[(MAX_BITS_PER_FRAME + 7) / 8] = {0}; Word16 count = 0;
    memcpy(input, pcm, sizeof(input));
    evs_enc(codec->encoder, input, FRAME_SAMPLES);
    indices_to_serial(codec->encoder, bits, &count); reset_indices_enc(codec->encoder);
    memset(input, 0, sizeof(input));
    if (count != 488) { codec->failed = 1; memset(bits, 0, sizeof(bits)); return BAD_STATE; }
    // Primary 24.4 kbit/s ToC: H=0, F=0, M=0, Q=0, frame type index=6.
    if (header_full) payload[0] = 6;
    memcpy(payload + header_full, bits, FRAME_BYTES); memset(bits, 0, sizeof(bits));
    return FRAME_BYTES + header_full;
}

/* Positive result is PCM sample count. Decode a validated packet into temporary PCM first. */
API int ivy_phone_evs_decode(void *handle, const uint8_t *payload, int bytes,
    int16_t *pcm, int capacity, int header_full_only) {
    PhoneEvs *codec = (PhoneEvs *)handle;
    if (!codec || !codec->ready || codec->failed || !codec->decoder) return BAD_STATE;
    if (!payload || !pcm || bytes < 1 || bytes > MAX_PAYLOAD || capacity < FRAME_SAMPLES ||
        (header_full_only != 0 && header_full_only != 1)) return BAD_ARGUMENT;
    struct { unsigned char *data; uint16_t bits, type; bool quality; } frames[MAX_FRAMES];
    // The reference RTP parser can rearrange AMR bits. Keep caller memory immutable.
    uint8_t packet[MAX_PAYLOAD]; memcpy(packet, payload, (size_t)bytes);
    bool more = true; int count = 0;
    while (more) {
        bool amr = false;
        if (count == MAX_FRAMES || !evsPayload_unpackFrame(header_full_only != 0,
            (const char *)packet, (uint16_t)bytes, (uint16_t)count, &amr, &more,
            &frames[count].type, &frames[count].quality, &frames[count].data, &frames[count].bits) || amr ||
            frames[count].type == 13) { memset(packet, 0, sizeof(packet)); return BAD_PAYLOAD; }
        count++;
    }
    if (capacity < count * FRAME_SAMPLES) { memset(packet, 0, sizeof(packet)); return BAD_ARGUMENT; }
    short decoded[MAX_FRAMES * FRAME_SAMPLES]; float output[L_FRAME48k];
    Decoder_State *state = codec->decoder; int result = count * FRAME_SAMPLES;
    for (int index = 0; index < count; index++) {
        read_indices_from_djb(state, frames[index].data, frames[index].bits, 0,
            (Word16)frames[index].type, frames[index].quality ? 1 : 0, 0, 0);
        if ((state->codec_mode != MODE1 && state->codec_mode != MODE2) || state->Opt_AMR_WB) { result = BAD_STATE; break; }
        evs_dec(state, output, state->bfi ? FRAMEMODE_MISSING : FRAMEMODE_NORMAL);
        syn_output(output, FRAME_SAMPLES, decoded + index * FRAME_SAMPLES);
        if (state->ini_frame < MAX_FRAME_COUNTER) state->ini_frame++;
    }
    if (result > 0) memcpy(pcm, decoded, (size_t)result * sizeof(short));
    else codec->failed = 1;
    memset(packet, 0, sizeof(packet)); memset(decoded, 0, sizeof(decoded)); memset(output, 0, sizeof(output));
    return result;
}
