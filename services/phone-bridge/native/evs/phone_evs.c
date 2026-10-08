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

enum { FRAME_SAMPLES = 960, MAX_FRAME_BYTES = 320, MAX_BLOCKS = 6, MAX_CHANNELS = 2, MAX_PAYLOAD = 4096 };
enum { BAD_ARGUMENT = -1, NO_MEMORY = -2, BAD_PAYLOAD = -3, BAD_STATE = -4 };
static const int rates[] = {7200, 8000, 9600, 13200, 16400, 24400, 32000, 48000, 64000, 96000, 128000};

typedef struct {
    Encoder_State *encoder[MAX_CHANNELS];
    Decoder_State *decoder[MAX_CHANNELS];
    Indice *indices[MAX_CHANNELS];
    int ready, failed, channels, bitrate;
} PhoneEvs;

API int ivy_phone_evs_abi(void) { return 2; }

API void ivy_phone_evs_destroy(void *handle) {
    PhoneEvs *codec = (PhoneEvs *)handle;
    if (!codec) return;
    for (int channel = 0; channel < MAX_CHANNELS; channel++) {
        if (channel < codec->ready) {
            if (codec->encoder[channel]) destroy_encoder(codec->encoder[channel]);
            if (codec->decoder[channel]) destroy_decoder(codec->decoder[channel]);
        }
        if (codec->encoder[channel]) { memset(codec->encoder[channel], 0, sizeof(Encoder_State)); free(codec->encoder[channel]); }
        if (codec->decoder[channel]) { memset(codec->decoder[channel], 0, sizeof(Decoder_State)); free(codec->decoder[channel]); }
        if (codec->indices[channel]) { memset(codec->indices[channel], 0, MAX_NUM_INDICES * sizeof(Indice)); free(codec->indices[channel]); }
    }
    memset(codec, 0, sizeof(*codec)); free(codec);
}

API int ivy_phone_evs_create(int encoding, int bitrate, int bandwidth, int channels, void **handle) {
    if (!handle) return BAD_ARGUMENT;
    *handle = NULL;
    int supported = 0;
    for (unsigned int index = 0; index < sizeof(rates) / sizeof(rates[0]); index++) if (rates[index] == bitrate) supported = 1;
    if ((encoding != 0 && encoding != 1) || channels < 1 || channels > MAX_CHANNELS || !supported ||
        bandwidth < NB || bandwidth > FB || (bandwidth == NB && bitrate > 24400) ||
        (bandwidth == SWB && bitrate < 9600) || (bandwidth == FB && bitrate < 16400)) return BAD_ARGUMENT;
    PhoneEvs *codec = (PhoneEvs *)calloc(1, sizeof(*codec));
    if (!codec) return NO_MEMORY;
    codec->channels = channels; codec->bitrate = bitrate;
    for (int channel = 0; channel < channels; channel++) {
        if (encoding) {
            codec->encoder[channel] = (Encoder_State *)calloc(1, sizeof(Encoder_State));
            codec->indices[channel] = (Indice *)calloc(MAX_NUM_INDICES, sizeof(Indice));
            if (!codec->encoder[channel] || !codec->indices[channel]) { ivy_phone_evs_destroy(codec); return NO_MEMORY; }
            Encoder_State *state = codec->encoder[channel];
            state->input_Fs = 48000; state->total_brate = bitrate; state->max_bwidth = bandwidth;
            state->Opt_AMR_WB = 0; state->Opt_DTX_ON = 0; state->Opt_RF_ON = 0;
            state->rf_fec_offset = 0; state->rf_fec_indicator = 1;
            state->interval_SID = FIXED_SID_RATE; state->var_SID_rate_flag = 1;
            state->Opt_SC_VBR = 0; state->last_Opt_SC_VBR = 0;
            state->bitstreamformat = MIME;
            state->codec_mode = (bitrate == 7200 || bitrate == 8000 || bitrate == 13200 || bitrate == 32000 || bitrate == 64000) ? MODE1 : MODE2;
            state->last_codec_mode = state->codec_mode; state->ind_list = codec->indices[channel];
            init_encoder(state);
        } else {
            codec->decoder[channel] = (Decoder_State *)calloc(1, sizeof(Decoder_State));
            if (!codec->decoder[channel]) { ivy_phone_evs_destroy(codec); return NO_MEMORY; }
            Decoder_State *state = codec->decoder[channel];
            state->output_Fs = 48000; state->codec_mode = 0; state->Opt_AMR_WB = 0;
            state->Opt_VOIP = 1; state->bitstreamformat = VOIP_RTPDUMP;
            state->sdp_hf_only = 0; state->amrwb_rfc4867_flag = -1;
            state->ini_frame = 0; state->prev_use_partial_copy = 0;
            init_decoder(state); reset_indices_dec(state);
        }
        codec->ready++;
    }
    *handle = codec; return 0;
}

/* Positive result is the exact payload length; errors never expose partial output. */
API int ivy_phone_evs_encode(void *handle, const int16_t *pcm, int samples,
    uint8_t *payload, int capacity, int header_full) {
    PhoneEvs *codec = (PhoneEvs *)handle;
    if (!codec || codec->ready != codec->channels || codec->failed || !codec->encoder[0]) return BAD_STATE;
    int frame_bytes = codec->bitrate / 400, headers = header_full ? codec->channels : 0;
    int bytes = codec->channels * frame_bytes + headers, type = -1;
    if (!pcm || !payload || samples != FRAME_SAMPLES || (header_full != 0 && header_full != 1) ||
        (codec->channels > 1 && !header_full) || capacity < bytes) return BAD_ARGUMENT;
    for (unsigned int index = 0; index < sizeof(rates) / sizeof(rates[0]); index++) if (rates[index] == codec->bitrate) type = (int)index + 1;
    short input[FRAME_SAMPLES]; UWord8 bits[MAX_CHANNELS][MAX_FRAME_BYTES] = {{0}};
    for (int channel = 0; channel < codec->channels; channel++) {
        Word16 count = 0;
        memcpy(input, pcm, sizeof(input));
        evs_enc(codec->encoder[channel], input, FRAME_SAMPLES);
        indices_to_serial(codec->encoder[channel], bits[channel], &count); reset_indices_enc(codec->encoder[channel]);
        memset(input, 0, sizeof(input));
        if (count != codec->bitrate / 50) { codec->failed = 1; memset(bits, 0, sizeof(bits)); return BAD_STATE; }
    }
    for (int channel = 0; channel < codec->channels; channel++) {
        // Header-Full ToCs precede all channel data, ordered by frame-block then channel.
        if (header_full) payload[channel] = (uint8_t)(type | (channel + 1 < codec->channels ? 0x40 : 0));
        memcpy(payload + headers + channel * frame_bytes, bits[channel], (size_t)frame_bytes);
    }
    memset(bits, 0, sizeof(bits)); return bytes;
}

/* Positive result is mono PCM sample count. Validate every channel/block before decoding. */
API int ivy_phone_evs_decode(void *handle, const uint8_t *payload, int bytes,
    int16_t *pcm, int capacity, int header_full_only) {
    PhoneEvs *codec = (PhoneEvs *)handle;
    if (!codec || codec->ready != codec->channels || codec->failed || !codec->decoder[0]) return BAD_STATE;
    if (!payload || !pcm || bytes < 1 || bytes > MAX_PAYLOAD || capacity < FRAME_SAMPLES ||
        (header_full_only != 0 && header_full_only != 1) || (codec->channels > 1 && !header_full_only)) return BAD_ARGUMENT;
    struct { unsigned char *data; uint16_t bits, type; bool quality; } frames[MAX_BLOCKS * MAX_CHANNELS];
    // The reference RTP parser can rearrange AMR bits. Keep caller memory immutable.
    uint8_t packet[MAX_PAYLOAD]; memcpy(packet, payload, (size_t)bytes);
    bool more = true; int count = 0;
    while (more) {
        bool amr = false;
        if (count == MAX_BLOCKS * codec->channels || !evsPayload_unpackFrame(header_full_only != 0,
            (const char *)packet, (uint16_t)bytes, (uint16_t)count, &amr, &more,
            &frames[count].type, &frames[count].quality, &frames[count].data, &frames[count].bits) || amr ||
            frames[count].type == 13) { memset(packet, 0, sizeof(packet)); return BAD_PAYLOAD; }
        count++;
    }
    if (count % codec->channels != 0) { memset(packet, 0, sizeof(packet)); return BAD_PAYLOAD; }
    int result = count / codec->channels * FRAME_SAMPLES;
    if (capacity < result) { memset(packet, 0, sizeof(packet)); return BAD_ARGUMENT; }
    short decoded[MAX_BLOCKS * FRAME_SAMPLES]; float output[L_FRAME48k], mixed[MAX_BLOCKS * FRAME_SAMPLES] = {0};
    for (int index = 0; index < count; index++) {
        Decoder_State *state = codec->decoder[index % codec->channels];
        read_indices_from_djb(state, frames[index].data, frames[index].bits, 0,
            (Word16)frames[index].type, frames[index].quality ? 1 : 0, 0, 0);
        if ((state->codec_mode != MODE1 && state->codec_mode != MODE2) || state->Opt_AMR_WB) { result = BAD_STATE; break; }
        evs_dec(state, output, state->bfi ? FRAMEMODE_MISSING : FRAMEMODE_NORMAL);
        int offset = index / codec->channels * FRAME_SAMPLES;
        for (int sample = 0; sample < FRAME_SAMPLES; sample++) mixed[offset + sample] += output[sample] / codec->channels;
        if (state->ini_frame < MAX_FRAME_COUNTER) state->ini_frame++;
    }
    if (result > 0) { syn_output(mixed, (Word16)result, decoded); memcpy(pcm, decoded, (size_t)result * sizeof(short)); }
    else codec->failed = 1;
    memset(packet, 0, sizeof(packet)); memset(decoded, 0, sizeof(decoded)); memset(output, 0, sizeof(output)); memset(mixed, 0, sizeof(mixed));
    return result;
}
