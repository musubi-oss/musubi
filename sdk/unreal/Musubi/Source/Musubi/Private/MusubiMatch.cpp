#include "MusubiMatch.h"
#include "MusubiMatches.h"
#include "MusubiSubsystem.h"
#include "Engine/GameInstance.h"
#include "Subsystems/SubsystemCollection.h"
#include "MusubiSocket.h"
#include "MusubiJson.h"
#include "MusubiModule.h"

void UMusubiMatch::Initialize(FSubsystemCollectionBase& Collection)
{
	Collection.InitializeDependency<UMusubiMatches>();
	Listen(EMusubiServerMessageTag::MatchInputAck, [this](const FMusubiServerMessage& M) { if (M.MatchInputAck.IsSet()) HandleAck(M.MatchInputAck.GetValue()); });
	Listen(EMusubiServerMessageTag::MatchTickHash, [this](const FMusubiServerMessage& M) { if (M.MatchTickHash.IsSet()) HandleHash(M.MatchTickHash.GetValue()); });
	Listen(EMusubiServerMessageTag::MatchState, [this](const FMusubiServerMessage& M) { if (M.MatchState.IsSet()) HandleState(M.MatchState.GetValue()); });
	Listen(EMusubiServerMessageTag::MatchStateDelta, [this](const FMusubiServerMessage& M) { if (M.MatchStateDelta.IsSet()) HandleDelta(M.MatchStateDelta.GetValue()); });
	Super::Initialize(Collection);
}

UMusubiMatches* UMusubiMatch::Matches() const
{
	UGameInstance* GI = GetGameInstance();
	return GI ? GI->GetSubsystem<UMusubiMatches>() : nullptr;
}

void UMusubiMatch::Attach(const FString& MatchId, double TickRate)
{
	FMusubiRollbackMatch& M = Rollback.FindOrAdd(MatchId);
	M.MatchId = MatchId;
	if (TickRate > 0) { M.TickRate = FMath::Clamp(TickRate, 1.0, 60.0); M.bTickRateMeasured = true; }
}

void UMusubiMatch::Detach(const FString& MatchId) { Rollback.Remove(MatchId); }

void UMusubiMatch::NoteServerTick(FMusubiRollbackMatch& M, uint64 Tick)
{
	if (Tick <= M.ServerTick) return;
	M.ServerTick = Tick;
	M.ServerTickAt = FPlatformTime::Seconds();
}

uint64 UMusubiMatch::ServerTickEstimate(const FString& MatchId) const
{
	const FMusubiRollbackMatch* M = Rollback.Find(MatchId);
	if (!M || M->ServerTick == 0) return 0;
	const double Elapsed = FPlatformTime::Seconds() - M->ServerTickAt;
	return M->ServerTick + (uint64)FMath::FloorToDouble(Elapsed * M->TickRate);
}

uint64 UMusubiMatch::PredictedTick(const FString& MatchId) const
{
	const FMusubiRollbackMatch* M = Rollback.Find(MatchId);
	if (!M) return 0;
	TSharedPtr<FMusubiSocket> S = Socket();
	const double RttMs = S.IsValid() ? S->GetStats().RttMs : 0.0;
	const double TickMs = 1000.0 / M->TickRate;
	const uint64 Lead = (uint64)FMath::CeilToDouble((RttMs * 0.5) / TickMs) + 1;
	return ServerTickEstimate(MatchId) + Lead;
}

uint64 UMusubiMatch::SendInput(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data)
{
	FMusubiRollbackMatch* M = Rollback.Find(MatchId);
	if (!M) return 0;
	const uint64 Tick = PredictedTick(MatchId);
	FMusubiClientMessage Msg;
	Msg.Tag = EMusubiClientMessageTag::MatchInput;
	FMusubiClientMessageMatchInput In;
	In.match_id = MatchId;
	In.tick = Tick;
	In.seq = M->NextSeq;
	In.op_code = OpCode;
	In.data = Data;
	Msg.MatchInput = MoveTemp(In);
	if (!Send(MoveTemp(Msg))) return 0;
	FMusubiPendingInput P;
	P.Tick = Tick; P.Seq = M->NextSeq++; P.OpCode = OpCode; P.Data = Data; P.SentAt = FPlatformTime::Seconds();
	M->Ring.Add(MoveTemp(P));
	while (M->Ring.Num() > FMath::Max(1, RingSize)) M->Ring.RemoveAt(0);
	M->Stats.InputsSent++;
	M->Stats.LeadTicks = (double)Tick - (double)ServerTickEstimate(MatchId);
	return Tick;
}

int64 UMusubiMatch::SendInputJson(const FString& MatchId, int64 OpCode, const FString& Json)
{
	FTCHARToUTF8 U(*Json);
	TArray<uint8> Bytes((const uint8*)U.Get(), U.Length());
	return (int64)SendInput(MatchId, OpCode, Bytes);
}

void UMusubiMatch::HandleAck(const FMusubiServerMessageMatchInputAck& A)
{
	FMusubiRollbackMatch* M = Rollback.Find(A.match_id);
	if (!M) return;
	NoteServerTick(*M, A.tick);
	M->Ring.RemoveAll([&](const FMusubiPendingInput& P) { return P.Seq <= A.seq; });
	M->LastAckedSeq = FMath::Max(M->LastAckedSeq, A.seq);
	M->Stats.InputsAcked++;
	if (!A.applied)
	{
		M->Stats.LateInputs++;
		OnInputRejectedNative.Broadcast(*M, A.tick);
		OnInputRejected.Broadcast(A.match_id, (int64)A.tick);
	}
}

void UMusubiMatch::HandleHash(const FMusubiServerMessageMatchTickHash& H)
{
	FMusubiRollbackMatch* M = Rollback.Find(H.match_id);
	if (!M) return;
	const double Now = FPlatformTime::Seconds();
	if (!M->bTickRateMeasured && M->LastHashTick && H.tick > M->LastHashTick && Now > M->LastHashAt)
	{
		// hashes arrive every hash_every ticks: ticks/sec = Δtick / Δt, smoothed
		const double Measured = (double)(H.tick - M->LastHashTick) / (Now - M->LastHashAt);
		if (Measured >= 1.0 && Measured <= 60.0) M->TickRate = M->TickRate * 0.8 + Measured * 0.2;
	}
	M->LastHashTick = H.tick;
	M->LastHashAt = Now;
	NoteServerTick(*M, H.tick);
	bool bMatched = true;
	if (HashTick.IsBound())
	{
		M->Stats.HashesChecked++;
		bMatched = HashTick.Execute(H.match_id, H.tick) == H.hash;
	}
	OnTickHashNative.Broadcast(*M, H.tick);
	OnTickHash.Broadcast(H.match_id, (int64)H.tick, bMatched);
	if (!bMatched) Desync(*M, H.tick, H.hash);
}

void UMusubiMatch::Desync(FMusubiRollbackMatch& M, uint64 Tick, uint64 ServerHash)
{
	M.Stats.Desyncs++;
	OnDesyncNative.Broadcast(M, Tick, ServerHash);
	OnDesync.Broadcast(M.MatchId, (int64)Tick);
	if (DesyncPolicy == EMusubiDesyncPolicy::Rejoin)
	{
		// there is no "send me a keyframe" message: re-joining yields a fresh match_state
		if (UMusubiMatches* Ms = Matches())
			if (const FMusubiMatchState* St = Ms->Match(M.MatchId))
			{
				FMusubiClientMessageMatchJoin Args = St->JoinArgs;
				const FString Id = M.MatchId;
				M.bAwaitingKeyframe = true;
				M.Ring.Reset();
				Ms->Leave(Id);
				Ms->Join(Args);
			}
	}
}

void UMusubiMatch::HandleState(const FMusubiServerMessageMatchState& S)
{
	FMusubiRollbackMatch* M = Rollback.Find(S.match_id);
	if (!M) return;
	NoteServerTick(*M, S.tick);
	M->State = MusubiJsonPatch::Clone(S.state.Value);
	M->StateTick = S.tick;
	M->bAwaitingKeyframe = false;
	M->Stats.Keyframes++;
	OnStateNative.Broadcast(*M, true);
	OnStateUpdated.Broadcast(S.match_id, (int64)S.tick, true);
}

void UMusubiMatch::HandleDelta(const FMusubiServerMessageMatchStateDelta& D)
{
	FMusubiRollbackMatch* M = Rollback.Find(D.match_id);
	if (!M) return;
	NoteServerTick(*M, D.tick);
	if (!M->State.IsValid() || M->bAwaitingKeyframe) return;
	TArray<TSharedPtr<FJsonValue>> Ops;
	for (const FMusubiJson& O : D.ops) Ops.Add(O.Value);
	FString Err;
	if (!MusubiJsonPatch::Apply(M->State, Ops, &Err))
	{
		M->Stats.PatchErrors++;
		UE_LOG(LogMusubi, Warning, TEXT("match %s: state delta at tick %llu failed (%s); waiting for a keyframe"), *D.match_id, D.tick, *Err);
		M->bAwaitingKeyframe = true;
		return;
	}
	M->StateTick = D.tick;
	M->Stats.Deltas++;
	OnStateNative.Broadcast(*M, false);
	OnStateUpdated.Broadcast(D.match_id, (int64)D.tick, false);
}

void UMusubiMatch::OnResubscribe()
{
	// a new session rejoins matches (UMusubiMatches) and gets a fresh keyframe; unacked inputs are stale
	for (auto& KV : Rollback) { KV.Value.Ring.Reset(); KV.Value.bAwaitingKeyframe = true; }
}

void UMusubiMatch::OnDisconnected() {}

FString UMusubiMatch::GetStateJson(const FString& MatchId) const
{
	const FMusubiRollbackMatch* M = Rollback.Find(MatchId);
	return M && M->State.IsValid() ? MusubiJson::Serialize(M->State) : FString();
}

FString UMusubiMatch::GetStateField(const FString& MatchId, const FString& Pointer) const
{
	const FMusubiRollbackMatch* M = Rollback.Find(MatchId);
	if (!M || !M->State.IsValid()) return FString();
	TSharedPtr<FJsonValue> V = MusubiJsonPatch::Get(M->State, Pointer);
	return V.IsValid() ? MusubiJson::Serialize(V) : FString();
}

int32 UMusubiMatch::PendingInputs(const FString& MatchId) const
{
	const FMusubiRollbackMatch* M = Rollback.Find(MatchId);
	return M ? M->Ring.Num() : 0;
}

int64 UMusubiMatch::LateInputs(const FString& MatchId) const
{
	const FMusubiRollbackMatch* M = Rollback.Find(MatchId);
	return M ? (int64)M->Stats.LateInputs : 0;
}
