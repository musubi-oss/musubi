// `SimCommandFrame { id?, #[serde(flatten)] cmd }`: a command on the stream is the command's own
// object plus an optional `id` the node echoes in its reply. Written by hand — the generator does
// not do `flatten`.
#pragma once

#include "CoreMinimal.h"
#include "MusubiSimWire.generated.h"

struct MUSUBISERVER_API FMusubiSimCommandFrame
{
	TOptional<FMusubiJson> Id;
	FMusubiSimCommand Cmd;

	static TSharedPtr<FJsonValue> ToJson(const FMusubiSimCommandFrame& V);
	static bool FromJson(const TSharedPtr<FJsonValue>& J, FMusubiSimCommandFrame& Out);
	FString ToJsonText() const { return MusubiJson::Serialize(ToJson(*this)); }
	bool FromJsonText(const FString& Text) { return FromJson(MusubiJson::Deserialize(Text), *this); }
};
