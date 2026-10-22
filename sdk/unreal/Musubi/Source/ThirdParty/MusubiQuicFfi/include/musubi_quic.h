/* C ABI of crates/musubi-quic-ffi (keep in sync with src/lib.rs; mq_abi_version() == MQ_ABI_VERSION). */
#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MQ_ABI_VERSION 1u

typedef struct MqConn MqConn;

enum MqEventKind { MQ_EVENT_NONE = 0, MQ_EVENT_CONNECTED = 1, MQ_EVENT_FRAME = 2, MQ_EVENT_CLOSED = 3, MQ_EVENT_ERROR = 4 };

/* `data` is valid until the next mq_poll / mq_close on the same handle. */
typedef struct MqEvent {
	uint32_t kind;       /* MqEventKind */
	uint8_t frame_type;  /* 0 text (JSON), 1 binary (postcard) */
	uint8_t unreliable;  /* 1 when the frame arrived as a datagram */
	uint32_t len;
	const uint8_t* data;
} MqEvent;

uint32_t mq_abi_version(void);
/* Returns NULL for invalid arguments; connect failures arrive as MQ_EVENT_ERROR. `root_pem` may be NULL. */
MqConn* mq_connect(const char* host, uint16_t port, const char* sni, int insecure, const char* root_pem);
int mq_send(MqConn* conn, uint8_t frame_type, const uint8_t* data, uint32_t len, int unreliable);
int mq_poll(MqConn* conn, MqEvent* out);
uint32_t mq_max_datagram(MqConn* conn);
void mq_close(MqConn* conn);

#ifdef __cplusplus
}
#endif
