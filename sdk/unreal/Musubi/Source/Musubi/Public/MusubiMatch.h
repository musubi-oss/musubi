// Rollback matches (docs/gdk-unreal.md §5) on top of UMusubiMatches: inputs stamped with a predicted
// tick and kept in a ring until acknowledged, the node's per-tick state hash checked against the
// project's deterministic simulation, and the synced match state (keyframe + RFC 6902 deltas) mirrored.
#pragma once

#include "MusubiFacade.h"
#include "MusubiJsonPatch.h"
#include "MusubiMatch.generated.h"

class UMusubiMatches;

/** FNV-1a 64 with the constants of `musubi_actor::state_hash` — hash exactly the bytes the server handler hashes. */
struct MUSUBI_API FMusubiFnv1a64
{
	uint64 H = 0xcbf29ce484222325ull;
	void Feed(const uint8* Data, int32 Len) { for (int32 i = 0; i < Len; ++i) { H ^= Data[i]; H *= 0x100000001b3ull; } }
	void Feed(const TArray<uint8>& B) { Feed(B.GetData(), B.Num()); }
	void FeedUtf8(const FString& S) { FTCHARToUTF8 U(*S); Feed((const uint8*)U.Get(), U.Length()); }
	uint64 Get() const { return H; }
	static uint64 Of(const TArray<uint8>& B) { FMusubiFnv1a64 F; F.Feed(B); return F.Get(); }
};

struct MUSUBI_API FMusubiPendingInput
{
	uint64 Tick = 0;
	uint64 Seq = 0;
	int64 OpCode = 0;
	TArray<uint8> Data;
	double SentAt = 0;
};

/** Per-match rollback state. */
struct MUSUBI_API FMusubiRollbackMatch
{
	FString MatchId;
	/** Authoritative ticks per second (match `tick_rate`, 1..=60); from Attach or measured from the hash cadence. */
	double TickRate = 30.0;
	bool bTickRateMeasured = false;
	/** Newest tick the node has told us about (ack / hash / state). */
	uint64 ServerTick = 0;
	double ServerTickAt = 0;
	uint64 NextSeq = 1;
	uint64 LastAckedSeq = 0;
	TArray<FMusubiPendingInput> Ring;
	/** Synced state: keyframe from `match_state`, patched by `match_state_delta`. */
	TSharedPtr<FJsonValue> State;
	uint64 StateTick = 0;
	uint64 LastHashTick = 0;
	double LastHashAt = 0;
	bool bAwaitingKeyframe = false;

	struct FStats { uint64 InputsSent = 0, InputsAcked = 0, LateInputs = 0, HashesChecked = 0, Desyncs = 0, Deltas = 0, Keyframes = 0, PatchErrors = 0; double LeadTicks = 0; } Stats;
};

DECLARE_DELEGATE_RetVal_TwoParams(uint64, FMusubiHashTick, const FString& /*MatchId*/, uint64 /*Tick*/);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiRollbackTickEvent, const FMusubiRollbackMatch&, uint64 /*Tick*/);
DECLARE_MULTICAST_DELEGATE_ThreeParams(FMusubiRollbackDesyncEvent, const FMusubiRollbackMatch&, uint64 /*Tick*/, uint64 /*ServerHash*/);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiRollbackStateEvent, const FMusubiRollbackMatch&, bool /*bKeyframe*/);

DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnMatchDesync, const FString&, MatchId, int64, Tick);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnInputRejected, const FString&, MatchId, int64, Tick);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FMusubiOnMatchStateUpdated, const FString&, MatchId, int64, Tick, bool, bKeyframe);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FMusubiOnMatchTickHash, const FString&, MatchId, int64, Tick, bool, bMatched);

UENUM(BlueprintType)
enum class EMusubiDesyncPolicy : uint8
{
	/** Only raise OnDesync; the project resyncs from State() itself. */
	Notify,
	/** Leave and re-join the match (fresh `match_state` keyframe). Inputs in flight are lost. */
	Rejoin,
};

UCLASS()
class MUSUBI_API UMusubiMatch : public UMusubiFacade
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	// ---- C++ API ----
	/** Start treating a joined match as rollback: `TickRate` = the match's `params.tick_rate` (0 = measure from the hash cadence, 30 until then). */
	void Attach(const FString& MatchId, double TickRate = 0.0);
	void Detach(const FString& MatchId);
	bool IsAttached(const FString& MatchId) const { return Rollback.Contains(MatchId); }
	const FMusubiRollbackMatch* Get(const FString& MatchId) const { return Rollback.Find(MatchId); }
	/** Send an input stamped with the predicted tick; returns that tick (0 when not sent). */
	uint64 SendInput(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data);
	/** The tick a frame sent now should target: ServerTick estimate + lead (ceil(rtt/2 / tick_ms) + 1). */
	uint64 PredictedTick(const FString& MatchId) const;
	uint64 ServerTickEstimate(const FString& MatchId) const;
	/** Project hook: hash of the project's predicted state after `Tick`, compared with `match_tick_hash`. Unbound = no check. */
	FMusubiHashTick HashTick;
	EMusubiDesyncPolicy DesyncPolicy = EMusubiDesyncPolicy::Notify;
	/** Ring capacity (inputs waiting for an ack); older entries are dropped. */
	int32 RingSize = 64;

	FMusubiRollbackTickEvent OnInputRejectedNative;
	FMusubiRollbackDesyncEvent OnDesyncNative;
	FMusubiRollbackStateEvent OnStateNative;
	FMusubiRollbackTickEvent OnTickHashNative;

	// ---- Blueprint ----
	UFUNCTION(BlueprintCallable, Category = "Musubi|Rollback") void AttachMatch(const FString& MatchId, float TickRate = 0.f) { Attach(MatchId, TickRate); }
	UFUNCTION(BlueprintCallable, Category = "Musubi|Rollback") void DetachMatch(const FString& MatchId) { Detach(MatchId); }
	UFUNCTION(BlueprintCallable, Category = "Musubi|Rollback") int64 SendInputBytes(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data) { return (int64)SendInput(MatchId, OpCode, Data); }
	UFUNCTION(BlueprintCallable, Category = "Musubi|Rollback") int64 SendInputJson(const FString& MatchId, int64 OpCode, const FString& Json);
	UFUNCTION(BlueprintPure, Category = "Musubi|Rollback") int64 GetPredictedTick(const FString& MatchId) const { return (int64)PredictedTick(MatchId); }
	UFUNCTION(BlueprintPure, Category = "Musubi|Rollback") int64 GetServerTick(const FString& MatchId) const { return (int64)ServerTickEstimate(MatchId); }
	UFUNCTION(BlueprintPure, Category = "Musubi|Rollback") FString GetStateJson(const FString& MatchId) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Rollback") FString GetStateField(const FString& MatchId, const FString& Pointer) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Rollback") int32 PendingInputs(const FString& MatchId) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Rollback") int64 LateInputs(const FString& MatchId) const;
	UFUNCTION(BlueprintCallable, Category = "Musubi|Rollback") void SetDesyncPolicy(EMusubiDesyncPolicy Policy) { DesyncPolicy = Policy; }
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Rollback") FMusubiOnMatchDesync OnDesync;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Rollback") FMusubiOnInputRejected OnInputRejected;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Rollback") FMusubiOnMatchStateUpdated OnStateUpdated;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Rollback") FMusubiOnMatchTickHash OnTickHash;

protected:
	virtual void OnResubscribe() override;
	virtual void OnDisconnected() override;

private:
	TMap<FString, FMusubiRollbackMatch> Rollback;
	UMusubiMatches* Matches() const;
	void NoteServerTick(FMusubiRollbackMatch& M, uint64 Tick);
	void HandleAck(const FMusubiServerMessageMatchInputAck& A);
	void HandleHash(const FMusubiServerMessageMatchTickHash& H);
	void HandleState(const FMusubiServerMessageMatchState& S);
	void HandleDelta(const FMusubiServerMessageMatchStateDelta& D);
	void Desync(FMusubiRollbackMatch& M, uint64 Tick, uint64 ServerHash);
};
