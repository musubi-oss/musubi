// Postcard (https://postcard.jamesmunns.com/wire-format) codec mirroring `musubi-proto`'s
// binary encoding: LEB128 varints, zigzag signed ints, little-endian f64, length-prefixed
// strings/bytes/sequences, `Option` as a 0/1 byte, enums as a varint variant index, and
// `uuid::Uuid` as 16 raw bytes (serde non-human-readable form).
#pragma once

#include "CoreMinimal.h"
#include "MusubiWireTypes.h"

class MUSUBI_API FMusubiPostcardWriter
{
public:
	explicit FMusubiPostcardWriter(int32 Reserve = 256) { Buf.Reserve(Reserve); }

	void Byte(uint8 B) { Buf.Add(B); }
	void Bool(bool B) { Buf.Add(B ? 1 : 0); }
	void Varint(uint64 V);
	void Zigzag(int64 V) { Varint(((uint64)V << 1) ^ (uint64)(V >> 63)); }
	void F64(double V);
	void Str(const FString& S);
	void Bytes(const TArray<uint8>& B);
	void Uuid(const FMusubiUuid& U);
	void Fail() { bFailed = true; }

	bool Ok() const { return !bFailed; }
	const TArray<uint8>& Data() const { return Buf; }
	TArray<uint8> Take() { return MoveTemp(Buf); }

private:
	TArray<uint8> Buf;
	bool bFailed = false;
};

class MUSUBI_API FMusubiPostcardReader
{
public:
	FMusubiPostcardReader(const uint8* InData, int32 InLen) : Data(InData), Size(InLen) {}
	explicit FMusubiPostcardReader(const TArray<uint8>& In) : Data(In.GetData()), Size(In.Num()) {}

	uint8 Byte();
	bool Bool() { return Byte() != 0; }
	uint64 Varint();
	int64 Zigzag() { const uint64 V = Varint(); return (int64)(V >> 1) ^ -(int64)(V & 1); }
	/** Sequence/map length, bounded so a corrupt frame cannot ask for gigabytes. */
	uint64 Len();
	double F64();
	FString Str();
	void Bytes(TArray<uint8>& Out);
	FMusubiUuid Uuid();
	void Fail() { bFailed = true; }

	bool Ok() const { return !bFailed; }
	/** True when every byte was consumed and nothing failed. */
	bool Finished() const { return !bFailed && Pos == Size; }
	int32 Remaining() const { return Size - Pos; }

	static constexpr uint64 MaxLen = 1u << 24;

private:
	const uint8* Data;
	int32 Size;
	int32 Pos = 0;
	bool bFailed = false;
};

namespace MusubiPostcard
{
	/** `pos_cm`: world positions travel as whole centimetres (i32) on the binary wire. */
	inline int32 ToCm(double V) { return (int32)FMath::Clamp(FMath::RoundToDouble(V * 100.0), (double)INT32_MIN, (double)INT32_MAX); }
	inline double FromCm(int32 Cm) { return (double)Cm / 100.0; }
}
