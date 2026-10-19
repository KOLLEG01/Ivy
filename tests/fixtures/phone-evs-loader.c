#define PHONE_EVS_BUILD
#include "phone_evs.h"
/* Loader rejection fixtures only; never used as a codec backend. */
API int ivy_phone_evs_abi(void) { return TEST_ABI; }
API int ivy_phone_evs_create(int e,int r,int b,int c,void **p) { *p=0; return -1; }
API void ivy_phone_evs_destroy(void *p) { (void)p; }
#ifndef TEST_MISSING
API int ivy_phone_evs_encode(void *h,const int16_t *p,int n,uint8_t *o,int c,int f) { return -4; }
#endif
API int ivy_phone_evs_decode(void *h,const uint8_t *p,int n,int16_t *o,int c,int f) { return -4; }
