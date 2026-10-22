#include "MusubiWireTypes.h"
#include "Misc/Guid.h"

bool FMusubiUuid::IsNil() const
{
	for (uint8 B : Bytes)
	{
		if (B) return false;
	}
	return true;
}

FString FMusubiUuid::ToString() const
{
	FString S;
	S.Reserve(36);
	static const TCHAR* Hex = TEXT("0123456789abcdef");
	for (int32 I = 0; I < 16; ++I)
	{
		if (I == 4 || I == 6 || I == 8 || I == 10) S.AppendChar(TEXT('-'));
		S.AppendChar(Hex[Bytes[I] >> 4]);
		S.AppendChar(Hex[Bytes[I] & 0xF]);
	}
	return S;
}

bool FMusubiUuid::Parse(const FString& Text, FMusubiUuid& Out)
{
	int32 N = 0;
	int32 Nibbles = 0;
	uint8 Cur = 0;
	for (TCHAR C : Text)
	{
		if (C == TEXT('-')) continue;
		int32 V;
		if (C >= TEXT('0') && C <= TEXT('9')) V = C - TEXT('0');
		else if (C >= TEXT('a') && C <= TEXT('f')) V = 10 + C - TEXT('a');
		else if (C >= TEXT('A') && C <= TEXT('F')) V = 10 + C - TEXT('A');
		else return false;
		Cur = (Cur << 4) | (uint8)V;
		if (++Nibbles == 2)
		{
			if (N >= 16) return false;
			Out.Bytes[N++] = Cur;
			Nibbles = 0;
			Cur = 0;
		}
	}
	return N == 16 && Nibbles == 0;
}

FMusubiUuid FMusubiUuid::NewV4()
{
	FMusubiUuid U;
	const FGuid G = FGuid::NewGuid();
	const uint32 Words[4] = { G.A, G.B, G.C, G.D };
	FMemory::Memcpy(U.Bytes, Words, 16);
	U.Bytes[6] = (U.Bytes[6] & 0x0F) | 0x40;
	U.Bytes[8] = (U.Bytes[8] & 0x3F) | 0x80;
	return U;
}
