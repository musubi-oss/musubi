#include "MusubiMassTypes.h"

FMusubiUuid FMusubiEntityFragment::Uuid() const
{
	FMusubiUuid U;
	for (int32 i = 0; i < 8; ++i)
	{
		U.Bytes[i] = (uint8)(IdHi >> (56 - 8 * i));
		U.Bytes[8 + i] = (uint8)(IdLo >> (56 - 8 * i));
	}
	return U;
}

void FMusubiEntityFragment::SetUuid(const FMusubiUuid& U)
{
	IdHi = IdLo = 0;
	for (int32 i = 0; i < 8; ++i)
	{
		IdHi = (IdHi << 8) | U.Bytes[i];
		IdLo = (IdLo << 8) | U.Bytes[8 + i];
	}
}
