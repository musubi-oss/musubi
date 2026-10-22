// Scalar wire types shared by the generated protocol structs.
#pragma once

#include "CoreMinimal.h"

/** A UUID exactly as Musubi sends it: 16 bytes on the binary wire, hyphenated lowercase text in JSON.
 *  (Not FGuid: FGuid's byte order and text form differ from RFC 4122.) */
struct MUSUBI_API FMusubiUuid
{
	uint8 Bytes[16] = {};

	bool IsNil() const;
	FString ToString() const;
	static bool Parse(const FString& Text, FMusubiUuid& Out);
	static FMusubiUuid NewV4();

	bool operator==(const FMusubiUuid& O) const { return FMemory::Memcmp(Bytes, O.Bytes, 16) == 0; }
	bool operator!=(const FMusubiUuid& O) const { return !(*this == O); }
	friend uint32 GetTypeHash(const FMusubiUuid& U) { uint32 H; FMemory::Memcpy(&H, U.Bytes, 4); return H; }
};
