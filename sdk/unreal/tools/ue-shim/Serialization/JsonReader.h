#pragma once
#include "CoreMinimal.h"
template <typename C = TCHAR>
class TJsonReader
{
public:
	FString Text;
	explicit TJsonReader(const FString& T) : Text(T) {}
};
template <typename C = TCHAR>
struct TJsonReaderFactory { static TSharedRef<TJsonReader<C>> Create(const FString& T) { return MakeShared<TJsonReader<C>>(T); } };
