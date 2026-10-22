// RFC 6902 JSON Patch over FJsonValue, for `match_state_delta.ops` (the node diffs the synced match
// state after every handler call). Supports add/remove/replace/move/copy/test with RFC 6901 pointers.
#pragma once

#include "CoreMinimal.h"
#include "Dom/JsonValue.h"
#include "Dom/JsonObject.h"

namespace MusubiJsonPatch
{
	/** Apply `Ops` to `Doc` in order; stops at the first failing op and returns false (Doc is then partially patched). */
	MUSUBI_API bool Apply(TSharedPtr<FJsonValue>& Doc, const TArray<TSharedPtr<FJsonValue>>& Ops, FString* Error = nullptr);
	/** Deep copy (patches must not alias into a previous snapshot). */
	MUSUBI_API TSharedPtr<FJsonValue> Clone(const TSharedPtr<FJsonValue>& V);
	/** Structural equality (numbers by value, objects key-wise, order-insensitive). */
	MUSUBI_API bool Equal(const TSharedPtr<FJsonValue>& A, const TSharedPtr<FJsonValue>& B);
	/** Resolve an RFC 6901 pointer; null when missing. */
	MUSUBI_API TSharedPtr<FJsonValue> Get(const TSharedPtr<FJsonValue>& Doc, const FString& Pointer);
}
