#pragma once
#include "CoreMinimal.h"
template <typename C> struct TCondensedJsonPrintPolicy {};
template <typename C = TCHAR, typename P = TCondensedJsonPrintPolicy<TCHAR>>
class TJsonWriter
{
public:
	FString* Out;
	explicit TJsonWriter(FString* O) : Out(O) {}
	void Close() {}
};
template <typename C = TCHAR, typename P = TCondensedJsonPrintPolicy<TCHAR>>
struct TJsonWriterFactory { static TSharedRef<TJsonWriter<C, P>> Create(FString* Out) { return MakeShared<TJsonWriter<C, P>>(Out); } };
