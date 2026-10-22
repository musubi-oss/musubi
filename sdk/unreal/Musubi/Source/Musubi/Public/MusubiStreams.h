// Streams facade: server-pushed stream frames (spectator feeds, replays, custom broadcasts) keyed by
// stream name (wire tag stream_data). Subscription is server-side (RPC/match join), so nothing is sent.
#pragma once

#include "MusubiFacade.h"
#include "MusubiStreams.generated.h"

DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiStreamDataEvent, const FMusubiServerMessageStreamData&);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FMusubiOnStreamData, const FString&, Stream, const FString&, DataJson, const FString&, SenderUsername);

UCLASS()
class MUSUBI_API UMusubiStreams : public UMusubiFacade
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	/** Per-stream listener; returns a handle to remove via `Unwatch`. */
	FDelegateHandle Watch(const FString& Stream, TFunction<void(const FMusubiServerMessageStreamData&)> Fn);
	void Unwatch(const FString& Stream, FDelegateHandle H);
	/** Frames received per stream since connect (for HUD/debug). */
	const TMap<FString, uint64>& Counters() const { return Frames; }

	FMusubiStreamDataEvent OnData;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Streams") FMusubiOnStreamData OnStreamData;

protected:
	virtual void OnDisconnected() override { Frames.Empty(); }

private:
	TMap<FString, FMusubiStreamDataEvent> PerStream;
	TMap<FString, uint64> Frames;
};
