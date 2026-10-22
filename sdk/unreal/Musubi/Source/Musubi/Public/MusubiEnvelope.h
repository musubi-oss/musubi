// Realtime frame = `Envelope { cid: Option<String>, msg }` in either encoding
// (`musubi-proto/src/lib.rs`). JSON: `{"cid":"1","msg":{"status_update":{...}}}`, unit variants
// `{"msg":"ping"}`. Postcard: cid as Option<String> then the message (varint variant index + fields).
#pragma once

#include "CoreMinimal.h"
#include "MusubiWire.generated.h"

enum class EMusubiEncoding : uint8
{
	Json,
	Postcard,
};

struct MUSUBI_API FMusubiClientEnvelope
{
	TOptional<FString> Cid;
	FMusubiClientMessage Msg;

	/** Text frame (JSON). */
	FString ToJsonText() const;
	/** Binary frame (postcard). Empty on failure (a data-carrying variant without its payload). */
	TArray<uint8> ToPostcard() const;
	bool FromJsonText(const FString& Text);
	bool FromPostcard(const uint8* Data, int32 Len);
};

struct MUSUBI_API FMusubiServerEnvelope
{
	TOptional<FString> Cid;
	FMusubiServerMessage Msg;

	FString ToJsonText() const;
	TArray<uint8> ToPostcard() const;
	bool FromJsonText(const FString& Text);
	bool FromPostcard(const uint8* Data, int32 Len);
};
