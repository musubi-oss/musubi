#include "MusubiPostcard.h"

void FMusubiPostcardWriter::Varint(uint64 V)
{
	while (V >= 0x80)
	{
		Buf.Add((uint8)(V & 0x7F) | 0x80);
		V >>= 7;
	}
	Buf.Add((uint8)V);
}

void FMusubiPostcardWriter::F64(double V)
{
	uint64 Bits;
	FMemory::Memcpy(&Bits, &V, 8);
	for (int32 I = 0; I < 8; ++I)
	{
		Buf.Add((uint8)(Bits >> (8 * I)));
	}
}

void FMusubiPostcardWriter::Str(const FString& S)
{
	FTCHARToUTF8 Utf8(*S);
	Varint((uint64)Utf8.Length());
	Buf.Append((const uint8*)Utf8.Get(), Utf8.Length());
}

void FMusubiPostcardWriter::Bytes(const TArray<uint8>& B)
{
	Varint((uint64)B.Num());
	Buf.Append(B);
}

// uuid's serde impl is `serialize_bytes`, so postcard writes a length prefix (16) first.
void FMusubiPostcardWriter::Uuid(const FMusubiUuid& U)
{
	Varint(16);
	Buf.Append(U.Bytes, 16);
}

uint8 FMusubiPostcardReader::Byte()
{
	if (Pos >= Size)
	{
		bFailed = true;
		return 0;
	}
	return Data[Pos++];
}

uint64 FMusubiPostcardReader::Varint()
{
	uint64 V = 0;
	for (int32 Shift = 0; Shift < 64; Shift += 7)
	{
		const uint8 B = Byte();
		V |= (uint64)(B & 0x7F) << Shift;
		if (!(B & 0x80) || bFailed)
		{
			return V;
		}
	}
	bFailed = true;
	return 0;
}

uint64 FMusubiPostcardReader::Len()
{
	const uint64 N = Varint();
	if (N > MaxLen || N > (uint64)Remaining())
	{
		bFailed = true;
		return 0;
	}
	return N;
}

double FMusubiPostcardReader::F64()
{
	if (Remaining() < 8)
	{
		bFailed = true;
		return 0.0;
	}
	uint64 Bits = 0;
	for (int32 I = 0; I < 8; ++I)
	{
		Bits |= (uint64)Data[Pos++] << (8 * I);
	}
	double V;
	FMemory::Memcpy(&V, &Bits, 8);
	return V;
}

FString FMusubiPostcardReader::Str()
{
	const uint64 N = Len();
	if (bFailed)
	{
		return FString();
	}
	FUTF8ToTCHAR Conv((const ANSICHAR*)(Data + Pos), (int32)N);
	Pos += (int32)N;
	return FString(Conv.Length(), Conv.Get());
}

void FMusubiPostcardReader::Bytes(TArray<uint8>& Out)
{
	const uint64 N = Len();
	Out.Reset((int32)N);
	if (bFailed)
	{
		return;
	}
	Out.Append(Data + Pos, (int32)N);
	Pos += (int32)N;
}

FMusubiUuid FMusubiPostcardReader::Uuid()
{
	FMusubiUuid U;
	if (Varint() != 16 || Remaining() < 16)
	{
		bFailed = true;
		return U;
	}
	FMemory::Memcpy(U.Bytes, Data + Pos, 16);
	Pos += 16;
	return U;
}
