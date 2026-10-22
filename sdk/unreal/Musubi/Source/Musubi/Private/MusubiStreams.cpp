#include "MusubiStreams.h"

void UMusubiStreams::Initialize(FSubsystemCollectionBase& Collection)
{
	Listen(EMusubiServerMessageTag::StreamData, [this](const FMusubiServerMessage& M) {
		if (!M.StreamData.IsSet()) return;
		const auto& D = M.StreamData.GetValue();
		Frames.FindOrAdd(D.stream)++;
		OnData.Broadcast(D);
		if (FMusubiStreamDataEvent* E = PerStream.Find(D.stream)) E->Broadcast(D);
		OnStreamData.Broadcast(D.stream, D.data.ToText(), D.sender.IsSet() ? D.sender->username : FString());
	});
	Super::Initialize(Collection);
}

FDelegateHandle UMusubiStreams::Watch(const FString& Stream, TFunction<void(const FMusubiServerMessageStreamData&)> Fn)
{
	return PerStream.FindOrAdd(Stream).AddLambda([Fn](const FMusubiServerMessageStreamData& D) { Fn(D); });
}

void UMusubiStreams::Unwatch(const FString& Stream, FDelegateHandle H)
{
	if (FMusubiStreamDataEvent* E = PerStream.Find(Stream)) E->Remove(H);
}
