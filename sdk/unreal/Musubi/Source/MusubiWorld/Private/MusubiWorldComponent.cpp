#include "MusubiWorldComponent.h"
#include "MusubiWorldModule.h"
#include "MusubiEntityComponent.h"
#include "MusubiSubsystem.h"
#include "MusubiSocket.h"
#include "MusubiJson.h"
#include "Components/InstancedStaticMeshComponent.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "Engine/StaticMesh.h"
#include "GameFramework/Actor.h"
#include "Dom/JsonObject.h"
#include "Dom/JsonValue.h"

// ---- FMusubiEntityRecord ----

void FMusubiEntityRecord::PushPos(uint64 Tick, const FVector& P)
{
	if (Ring.Num() && Ring.Last().Tick == Tick) { Ring.Last().Pos = P; return; }
	if (Ring.Num() && Ring.Last().Tick > Tick) return;
	if (Ring.Num() >= 4) Ring.RemoveAt(0);
	Ring.Add({Tick, P});
}

FVector FMusubiEntityRecord::PosAt(double T, double MaxExtrapolateTicks) const
{
	if (Ring.Num() == 0) return RenderPos;
	if (Ring.Num() == 1 || T <= (double)Ring[0].Tick) return Ring[0].Pos;
	const FMusubiPosSample& Newest = Ring.Last();
	if (T >= (double)Newest.Tick)
	{
		const FMusubiPosSample& Prev = Ring[Ring.Num() - 2];
		const double Span = (double)(Newest.Tick - Prev.Tick);
		if (Span <= 0) return Newest.Pos;
		const double Ahead = FMath::Min(T - (double)Newest.Tick, MaxExtrapolateTicks);
		return Newest.Pos + (Newest.Pos - Prev.Pos) * (Ahead / Span);
	}
	for (int32 i = 1; i < Ring.Num(); ++i)
	{
		if (T <= (double)Ring[i].Tick)
		{
			const FMusubiPosSample& A = Ring[i - 1];
			const FMusubiPosSample& B = Ring[i];
			const double Span = (double)(B.Tick - A.Tick);
			const double F = Span > 0 ? (T - (double)A.Tick) / Span : 1.0;
			return FMath::Lerp(A.Pos, B.Pos, F);
		}
	}
	return Newest.Pos;
}

// ---- lifecycle ----

UMusubiWorldComponent::UMusubiWorldComponent()
{
	PrimaryComponentTick.bCanEverTick = true;
	PrimaryComponentTick.TickGroup = TG_PrePhysics;
}

void UMusubiWorldComponent::BeginPlay()
{
	Super::BeginPlay();
	TickHz = NominalTickHz;
	StatsWindowStart = FPlatformTime::Seconds();
	Bind();
	if (bAutoSubscribe && !WorldName.IsEmpty()) Subscribe();
}

void UMusubiWorldComponent::EndPlay(const EEndPlayReason::Type Reason)
{
	if (bSubscribed) Unsubscribe();
	Unbind();
	ResetRegistry();
	for (auto& P : Pool) for (TWeakObjectPtr<AActor>& A : P.Value) if (A.IsValid()) A->Destroy();
	Pool.Reset();
	Instanced.Reset();
	Super::EndPlay(Reason);
}

UMusubiSubsystem* UMusubiWorldComponent::Musubi() const
{
	UWorld* W = GetWorld();
	UGameInstance* GI = W ? W->GetGameInstance() : nullptr;
	return GI ? GI->GetSubsystem<UMusubiSubsystem>() : nullptr;
}

TSharedPtr<FMusubiSocket> UMusubiWorldComponent::Socket()
{
	Bind();
	return Bound.Pin();
}

void UMusubiWorldComponent::Bind()
{
	if (Bound.IsValid()) return;
	UMusubiSubsystem* M = Musubi();
	TSharedPtr<FMusubiSocket> S = M ? M->Socket() : nullptr;
	if (!S.IsValid()) return;
	Bound = S;
	TWeakObjectPtr<UMusubiWorldComponent> Weak(this);
	DeltaHandle = S->OnTag(EMusubiServerMessageTag::WorldDelta).AddLambda([Weak](const FMusubiServerEnvelope& E) {
		if (Weak.IsValid() && E.Msg.WorldDelta.IsSet()) Weak->HandleDelta(E.Msg.WorldDelta.GetValue());
	});
	ResubHandle = S->OnResubscribe.AddLambda([Weak]() { if (Weak.IsValid()) Weak->HandleReconnect(); });
	ClosedHandle = S->OnClosed.AddLambda([Weak](int32, const FString&) { if (Weak.IsValid()) Weak->HandleClosed(); });
}

void UMusubiWorldComponent::Unbind()
{
	if (TSharedPtr<FMusubiSocket> S = Bound.Pin())
	{
		S->OnTag(EMusubiServerMessageTag::WorldDelta).Remove(DeltaHandle);
		S->OnResubscribe.Remove(ResubHandle);
		S->OnClosed.Remove(ClosedHandle);
	}
	Bound.Reset();
}

// ---- subscription driver ----

FVector UMusubiWorldComponent::InterestPos() const
{
	if (bInterestOverride) return InterestOverride;
	const AActor* O = GetOwner();
	return O ? O->GetActorLocation() : FVector::ZeroVector;
}

float UMusubiWorldComponent::BaseRadius() const
{
	return FMath::Max(ViewDistance, 2.f * CellSize);
}

void UMusubiWorldComponent::Subscribe()
{
	bWantSubscribed = true;
	if (EffectiveRadius <= 0.f) EffectiveRadius = BaseRadius();
	SendSubscribe(true);
}

void UMusubiWorldComponent::Unsubscribe()
{
	bWantSubscribed = false;
	if (TSharedPtr<FMusubiSocket> S = Socket(); S.IsValid() && S->IsConnected() && bSubscribed)
	{
		FMusubiClientMessage M;
		M.Tag = EMusubiClientMessageTag::WorldUnsubscribe;
		FMusubiClientMessageWorldUnsubscribe B;
		B.world = WorldName;
		M.WorldUnsubscribe = MoveTemp(B);
		S->Send(M);
	}
	bSubscribed = false;
	Stats.bSubscribed = false;
	ResetRegistry();
}

void UMusubiWorldComponent::SendSubscribe(bool bForce)
{
	TSharedPtr<FMusubiSocket> S = Socket();
	if (!S.IsValid() || !S->IsConnected() || WorldName.IsEmpty()) return;
	const FVector P = InterestPos();
	const double Now = FPlatformTime::Seconds();
	if (!bForce)
	{
		const float MoveThreshold = ResubscribeDistance > 0.f ? ResubscribeDistance : EffectiveRadius * 0.25f;
		const bool bMoved = FVector::Dist2D(P, LastSentPos) > MoveThreshold;
		const bool bDue = Now - LastSentAt > ResubscribeIntervalSec;
		if (!bMoved && !bDue) return;
	}
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::WorldSubscribe;
	FMusubiClientMessageWorldSubscribe B;
	B.world = WorldName;
	B.x = P.X;
	B.y = P.Y;
	B.radius = EffectiveRadius;
	if (ClientBudgetBytes > 0) B.budget_bytes = (uint32)ClientBudgetBytes;
	if (Filter.Num())
	{
		TSharedPtr<FJsonObject> F = MakeShared<FJsonObject>();
		for (const auto& KV : Filter)
		{
			TSharedPtr<FJsonValue> V = MusubiJson::Deserialize(KV.Value);
			F->SetField(KV.Key, V.IsValid() ? V : TSharedPtr<FJsonValue>(MakeShared<FJsonValueString>(KV.Value)));
		}
		FMusubiJson J;
		J.Value = MakeShared<FJsonValueObject>(F);
		B.filter = J;
	}
	int32 Hz = MaxHz;
	if (Hz <= 0)
	{
		const float Fps = GetWorld() && GetWorld()->GetDeltaSeconds() > 0.f ? 1.f / GetWorld()->GetDeltaSeconds() : 60.f;
		Hz = FMath::Clamp(FMath::RoundToInt(FMath::Min(Fps, 30.f)), 1, 30);
	}
	B.max_hz = (uint32)Hz;
	B.unreliable_updates = bUnreliableUpdates;
	B.inside = Inside;
	M.WorldSubscribe = MoveTemp(B);
	if (S->Send(M))
	{
		LastSentPos = P;
		LastSentAt = Now;
		bSubscribed = true;
		Stats.bSubscribed = true;
		Stats.Radius = EffectiveRadius;
	}
}

void UMusubiWorldComponent::OpenContainer(const FString& EntityId)
{
	Inside = EntityId;
	if (bSubscribed) SendSubscribe(true);
}

void UMusubiWorldComponent::CloseContainer()
{
	Inside.Reset();
	if (bSubscribed) SendSubscribe(true);
}

void UMusubiWorldComponent::SetLocalEntity(const FString& EntityId)
{
	FMusubiUuid U;
	if (EntityId.IsEmpty()) { LocalEntity.Reset(); return; }
	if (!FMusubiUuid::Parse(EntityId, U)) { UE_LOG(LogMusubiWorld, Warning, TEXT("SetLocalEntity: bad uuid %s"), *EntityId); return; }
	LocalEntity = U;
	if (FMusubiEntityRecord* R = Registry.Find(U))
		if (UMusubiEntityComponent* C = R->Actor.IsValid() ? EntityComponentOf(R->Actor.Get(), false) : nullptr) C->Reset(U, true);
}

bool UMusubiWorldComponent::SendInput(const FMusubiJson& Input)
{
	TSharedPtr<FMusubiSocket> S = Socket();
	if (!S.IsValid() || !S->IsConnected() || !LocalEntity.IsSet()) return false;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::WorldInput;
	FMusubiClientMessageWorldInput B;
	B.world = WorldName;
	B.entity = LocalEntity->ToString();
	B.input = Input;
	M.WorldInput = MoveTemp(B);
	return S->Send(M);
}

bool UMusubiWorldComponent::SendInputJson(const FString& InputJson)
{
	return SendInput(FMusubiJson::FromText(InputJson));
}

void UMusubiWorldComponent::SetInterestOverride(FVector WorldPos, bool bEnable)
{
	bInterestOverride = bEnable;
	InterestOverride = WorldPos;
}

// ---- socket events ----

void UMusubiWorldComponent::HandleReconnect()
{
	if (!bWantSubscribed) return;
	// a new session has a fresh interest on the node: everything in range comes back as `spawned`,
	// possibly over several ticks when the budget defers part of it
	for (auto& KV : Registry) KV.Value.bStale = true;
	bAwaitingKeyframe = true;
	KeyframeDeadline = FPlatformTime::Seconds() + ReconnectOrphanGraceSec;
	LastSequencedTick = 0;
	SendSubscribe(true);
}

void UMusubiWorldComponent::HandleClosed()
{
	bSubscribed = false;
	Stats.bSubscribed = false;
}

void UMusubiWorldComponent::HandleDelta(const FMusubiServerMessageWorldDelta& D)
{
	if (D.world != WorldName) return;
	if (D.sequenced)
	{
		if (D.tick <= LastSequencedTick || D.tick <= LastReliableTick) { LateThisSec++; return; }
		LastSequencedTick = D.tick;
		SequencedThisSec++;
		for (const FMusubiWorldEntity& E : D.updated) ApplyUpdate(E, D.tick, true);
	}
	else
	{
		if (D.tick < LastReliableTick) { LateThisSec++; return; }
		LastReliableTick = D.tick;
		if (D.tick >= LastSequencedTick) LastSequencedTick = D.tick;
		DeltasThisSec++;
		for (const FMusubiUuid& Id : D.despawned) ApplyDespawn(Id);
		for (const FMusubiWorldEntity& E : D.spawned) ApplySpawn(E, D.tick);
		for (const FMusubiWorldEntity& E : D.updated) ApplyUpdate(E, D.tick, false);
		if (PendingUnknown.Num())
		{
			for (int32 i = PendingUnknown.Num() - 1; i >= 0; --i)
			{
				FPendingUpdate& P = PendingUnknown[i];
				if (Registry.Contains(P.Entity.id)) { ApplyUpdate(P.Entity, P.Tick, false); PendingUnknown.RemoveAtSwap(i); }
				else if (D.tick > P.ExpiresTick) { Stats.DroppedUnknownUpdates++; PendingUnknown.RemoveAtSwap(i); }
			}
		}
	}
	LastTick = FMath::Max(LastTick, D.tick);
	Stats.LastTick = (int64)LastTick;
	NoteTickTiming(D.tick);
	const float Scale = (float)D.time_scale;
	if (!FMath::IsNearlyEqual(Scale, TimeScale, 0.001f))
	{
		TimeScale = Scale;
		Stats.TimeScale = Scale;
		OnTimeScaleChanged.Broadcast(Scale);
	}
	OnDeltaApplied.Broadcast(D);
}

void UMusubiWorldComponent::NoteTickTiming(uint64 Tick)
{
	const double Now = FPlatformTime::Seconds();
	if (LastDeltaWallTime > 0 && Tick > LastDeltaTick)
	{
		const double Dt = Now - LastDeltaWallTime;
		const double Ticks = (double)(Tick - LastDeltaTick);
		if (Dt > 0.001 && Ticks <= 120)
		{
			// wall-clock ticks/s; while the world is dilated the node ticks slower, so divide the
			// dilation back out to recover the nominal rate the playback clock runs against
			const double Measured = Ticks / Dt / FMath::Max(0.05, (double)TimeScale);
			TickHz = TickHz <= 0 ? Measured : TickHz * 0.9 + Measured * 0.1;
			Stats.TickHz = (float)TickHz;
		}
	}
	else if (LastDeltaWallTime < 0)
	{
		PlaybackTick = (double)Tick - InterpolationDelayTicks;
	}
	LastDeltaWallTime = Now;
	LastDeltaTick = Tick;
}

// ---- registry ----

FVector UMusubiWorldComponent::WirePos(const FMusubiEntityRecord& R, const FMusubiWorldEntity& E) const
{
	double Z = DefaultZ;
	if (!HeightComponent.IsEmpty())
	{
		const FMusubiWorldComponent* C = E.components.FindByPredicate([&](const FMusubiWorldComponent& X) { return X.name == HeightComponent; });
		double V;
		if (C && C->value.Value.IsValid() && C->value.Value->TryGetNumber(V)) Z = V;
		else if (const FMusubiEntityComponentState* K = R.FindComponent(HeightComponent))
			if (K->Value.Value.IsValid() && K->Value.Value->TryGetNumber(V)) Z = V;
	}
	return FVector(E.x, E.y, Z);
}

void UMusubiWorldComponent::ApplyComponents(FMusubiEntityRecord& R, const FMusubiWorldEntity& E, UMusubiEntityComponent* Comp)
{
	for (const FMusubiWorldComponent& C : E.components)
	{
		FMusubiEntityComponentState* Known = R.Components.Find(C.name);
		if (Known && C.version <= Known->Version && C.version != 0) { Stats.StaleComponentUpdates++; continue; }
		FMusubiEntityComponentState& S = R.Components.FindOrAdd(C.name);
		S.Value = C.value;
		S.Version = C.version;
		if (Comp) Comp->SetComponent(C.name, C.value, C.version);
	}
}

void UMusubiWorldComponent::ApplySpawn(const FMusubiWorldEntity& E, uint64 Tick)
{
	FMusubiEntityRecord* Existing = Registry.Find(E.id);
	const bool bResync = Existing != nullptr;
	FMusubiEntityRecord& R = Existing ? *Existing : Registry.Add(E.id);
	R.Id = E.id;
	R.bStale = false;
	R.LastTick = Tick;
	if (bResync)
	{
		// re-sync: the node sends the full component set; drop what it no longer lists
		R.Components.Reset();
	}
	const FVector P = WirePos(R, E);
	if (!bResync) { R.Ring.Reset(); R.RenderPos = P; }
	R.PushPos(Tick, P);
	UMusubiEntityComponent* Comp = nullptr;
	if (!bResync || !R.Actor.IsValid()) Materialize(R, E);
	if (R.Actor.IsValid())
	{
		Comp = EntityComponentOf(R.Actor.Get(), true);
		if (Comp) Comp->Reset(E.id, LocalEntity.IsSet() && LocalEntity.GetValue() == E.id);
	}
	ApplyComponents(R, E, Comp);
	if (Comp) { Comp->SetLastTick(Tick); Comp->NotifyReady(); }
	OnRecordSpawned.Broadcast(R, !bResync);
	if (!bResync) OnEntitySpawned.Broadcast(E.id.ToString(), R.Actor.Get());
}

bool UMusubiWorldComponent::ApplyUpdate(const FMusubiWorldEntity& E, uint64 Tick, bool bSequenced)
{
	FMusubiEntityRecord* R = Registry.Find(E.id);
	if (!R)
	{
		if (bSequenced) return false;
		if (UnknownUpdateGraceTicks > 0 && !PendingUnknown.ContainsByPredicate([&](const FPendingUpdate& P) { return P.Entity.id == E.id && P.Tick >= Tick; }))
			PendingUnknown.Add({E, Tick, Tick + (uint64)UnknownUpdateGraceTicks});
		else if (UnknownUpdateGraceTicks <= 0) Stats.DroppedUnknownUpdates++;
		return false;
	}
	if (Tick < R->LastTick) return false;
	R->bStale = false;
	R->LastTick = Tick;
	R->PushPos(Tick, WirePos(*R, E));
	if (!bSequenced && E.components.Num())
	{
		UMusubiEntityComponent* Comp = R->Actor.IsValid() ? EntityComponentOf(R->Actor.Get(), false) : nullptr;
		ApplyComponents(*R, E, Comp);
		if (Comp) Comp->SetLastTick(Tick);
	}
	return true;
}

void UMusubiWorldComponent::ApplyDespawn(const FMusubiUuid& Id)
{
	FMusubiEntityRecord* R = Registry.Find(Id);
	if (!R) return;
	Dematerialize(*R);
	Registry.Remove(Id);
	OnRecordDespawned.Broadcast(Id);
	OnEntityDespawned.Broadcast(Id.ToString());
}

void UMusubiWorldComponent::ResetRegistry()
{
	for (auto& KV : Registry) Dematerialize(KV.Value);
	Registry.Reset();
	PendingUnknown.Reset();
	LastReliableTick = LastSequencedTick = LastTick = 0;
	LastDeltaWallTime = -1;
	bAwaitingKeyframe = false;
}

// ---- sinks ----

UMusubiEntityComponent* UMusubiWorldComponent::EntityComponentOf(AActor* A, bool bCreate)
{
	if (!A) return nullptr;
	if (UMusubiEntityComponent* C = A->FindComponentByClass<UMusubiEntityComponent>()) return C;
	if (!bCreate) return nullptr;
	UMusubiEntityComponent* C = NewObject<UMusubiEntityComponent>(A, TEXT("MusubiEntity"));
	A->AddInstanceComponent(C);
	C->RegisterComponent();
	return C;
}

AActor* UMusubiWorldComponent::AcquireActor(UClass* Class, const FVector& At)
{
	if (TArray<TWeakObjectPtr<AActor>>* P = Pool.Find(Class))
	{
		while (P->Num())
		{
			TWeakObjectPtr<AActor> A = P->Pop();
			if (!A.IsValid()) continue;
			A->SetActorLocation(At, false, nullptr, ETeleportType::TeleportPhysics);
			A->SetActorHiddenInGame(false);
			A->SetActorEnableCollision(true);
			A->SetActorTickEnabled(true);
			Stats.PooledActors--;
			return A.Get();
		}
	}
	UWorld* W = GetWorld();
	if (!W) return nullptr;
	FActorSpawnParameters SP;
	SP.SpawnCollisionHandlingOverride = ESpawnActorCollisionHandlingMethod::AlwaysSpawn;
	SP.ObjectFlags |= RF_Transient;
	const FTransform T(FRotator::ZeroRotator, At);
	return W->SpawnActor(Class, &T, SP);
}

void UMusubiWorldComponent::ReleaseActor(AActor* A)
{
	if (!A) return;
	UClass* C = A->GetClass();
	TArray<TWeakObjectPtr<AActor>>& P = Pool.FindOrAdd(C);
	if (PoolSize > 0 && P.Num() < PoolSize)
	{
		A->SetActorHiddenInGame(true);
		A->SetActorEnableCollision(false);
		A->SetActorTickEnabled(false);
		P.Add(A);
		Stats.PooledActors++;
	}
	else A->Destroy();
}

UMusubiWorldComponent::FInstancedSink& UMusubiWorldComponent::SinkFor(UStaticMesh* Mesh, float Scale)
{
	FInstancedSink& S = Instanced.FindOrAdd(Mesh);
	if (!S.Ism.IsValid())
	{
		AActor* O = GetOwner();
		UInstancedStaticMeshComponent* Ism = NewObject<UInstancedStaticMeshComponent>(O, NAME_None, RF_Transient);
		Ism->SetStaticMesh(Mesh);
		Ism->SetMobility(EComponentMobility::Movable);
		Ism->SetCollisionEnabled(ECollisionEnabled::NoCollision);
		Ism->NumCustomDataFloats = 0;
		Ism->RegisterComponentWithWorld(GetWorld());
		Ism->SetWorldTransform(FTransform::Identity);
		S.Ism = Ism;
		S.Scale = Scale;
		S.FreeSlots.Reset();
		S.SlotOwner.Reset();
	}
	return S;
}

void UMusubiWorldComponent::Materialize(FMusubiEntityRecord& R, const FMusubiWorldEntity& E)
{
	const FMusubiEntityRule* Rule = Mapping ? Mapping->Resolve(E) : nullptr;
	EMusubiEntitySink Sink = EMusubiEntitySink::None;
	UClass* Class = nullptr;
	UStaticMesh* Mesh = nullptr;
	float Scale = 1.f;
	if (Rule)
	{
		Sink = Rule->Sink;
		Class = Rule->ActorClass.Get();
		Mesh = Rule->Mesh.Get();
		Scale = Rule->Scale;
	}
	else if (Mapping && Mapping->DefaultActorClass)
	{
		Sink = EMusubiEntitySink::Actor;
		Class = Mapping->DefaultActorClass.Get();
	}
	if (Sink == EMusubiEntitySink::Actor && !Class) Sink = EMusubiEntitySink::None;
	if (Sink == EMusubiEntitySink::Instanced && !Mesh) Sink = EMusubiEntitySink::None;
	R.Sink = Sink;
	if (Sink == EMusubiEntitySink::Actor)
	{
		R.Actor = AcquireActor(Class, R.RenderPos);
		Stats.Actors++;
	}
	else if (Sink == EMusubiEntitySink::Instanced)
	{
		FInstancedSink& S = SinkFor(Mesh, Scale);
		UInstancedStaticMeshComponent* Ism = S.Ism.Get();
		const FTransform T(FRotator::ZeroRotator, R.RenderPos, FVector(Scale));
		int32 Slot;
		if (S.FreeSlots.Num())
		{
			Slot = S.FreeSlots.Pop();
			Ism->UpdateInstanceTransform(Slot, T, true, false, true);
		}
		else
		{
			Slot = Ism->AddInstance(T, true);
			S.SlotOwner.SetNum(FMath::Max(S.SlotOwner.Num(), Slot + 1));
		}
		S.SlotOwner[Slot] = E.id;
		R.Mesh = Mesh;
		R.InstanceSlot = Slot;
		Stats.Instances++;
	}
}

void UMusubiWorldComponent::Dematerialize(FMusubiEntityRecord& R)
{
	if (R.Actor.IsValid())
	{
		if (UMusubiEntityComponent* C = EntityComponentOf(R.Actor.Get(), false)) C->NotifyDespawned();
		ReleaseActor(R.Actor.Get());
		Stats.Actors--;
	}
	R.Actor.Reset();
	if (R.InstanceSlot != INDEX_NONE)
	{
		if (FInstancedSink* S = R.Mesh.IsValid() ? Instanced.Find(R.Mesh.Get()) : nullptr)
		{
			if (UInstancedStaticMeshComponent* Ism = S->Ism.Get())
				Ism->UpdateInstanceTransform(R.InstanceSlot, FTransform(FRotator::ZeroRotator, FVector(0, 0, -1e7), FVector::ZeroVector), true, true, true);
			S->FreeSlots.Add(R.InstanceSlot);
			if (S->SlotOwner.IsValidIndex(R.InstanceSlot)) S->SlotOwner[R.InstanceSlot] = FMusubiUuid();
		}
		R.InstanceSlot = INDEX_NONE;
		Stats.Instances--;
	}
	R.Sink = EMusubiEntitySink::None;
}

// ---- per frame ----

void UMusubiWorldComponent::TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction)
{
	Super::TickComponent(DeltaTime, TickType, ThisTickFunction);
	const double Now = FPlatformTime::Seconds();
	if (bWantSubscribed)
	{
		if (!bSubscribed) SendSubscribe(true);
		else SendSubscribe(false);
	}
	if (bAwaitingKeyframe && Now > KeyframeDeadline)
	{
		bAwaitingKeyframe = false;
		TArray<FMusubiUuid> Orphans;
		for (const auto& KV : Registry) if (KV.Value.bStale) Orphans.Add(KV.Key);
		for (const FMusubiUuid& Id : Orphans) ApplyDespawn(Id);
	}
	Interpolate(DeltaTime);
	BandwidthGuard(DeltaTime);
	if (Now - StatsWindowStart >= 1.0) RollStats(Now);
}

void UMusubiWorldComponent::Interpolate(float DeltaTime)
{
	if (LastDeltaWallTime < 0) return;
	// the playback clock runs in authoritative ticks and follows the node's dilation so slowed
	// cells play slow instead of stuttering; it is pulled toward (newest tick − delay) when it drifts
	PlaybackTick += (double)DeltaTime * TickHz * (double)TimeScale;
	const double Target = (double)LastTick - (double)InterpolationDelayTicks
		+ (FPlatformTime::Seconds() - LastDeltaWallTime) * TickHz * (double)TimeScale;
	const double Drift = Target - PlaybackTick;
	if (FMath::Abs(Drift) > 4.0) PlaybackTick = Target;
	else PlaybackTick += Drift * FMath::Min(1.0, (double)DeltaTime * 2.0);

	for (auto& KV : Registry)
	{
		FMusubiEntityRecord& R = KV.Value;
		if (R.Sink == EMusubiEntitySink::None || R.Ring.Num() == 0) continue;
		const FVector Prev = R.RenderPos;
		R.RenderPos = R.PosAt(PlaybackTick, MaxExtrapolateTicks);
		if (R.Sink == EMusubiEntitySink::Actor && R.Actor.IsValid())
		{
			AActor* A = R.Actor.Get();
			FRotator Rot = A->GetActorRotation();
			double Yaw;
			const FMusubiEntityComponentState* YawC = YawComponent.IsEmpty() ? nullptr : R.FindComponent(YawComponent);
			if (YawC && YawC->Value.Value.IsValid() && YawC->Value.Value->TryGetNumber(Yaw)) Rot.Yaw = (float)Yaw;
			else if (FVector::DistSquared2D(R.RenderPos, Prev) > 1.0) Rot.Yaw = (R.RenderPos - Prev).Rotation().Yaw;
			A->SetActorLocationAndRotation(R.RenderPos, Rot, false, nullptr, ETeleportType::None);
		}
	}
	FlushInstancedTransforms();
}

void UMusubiWorldComponent::FlushInstancedTransforms()
{
	for (auto& KV : Instanced)
	{
		FInstancedSink& S = KV.Value;
		UInstancedStaticMeshComponent* Ism = S.Ism.Get();
		if (!Ism || S.SlotOwner.Num() == 0) continue;
		IsmScratch.SetNum(S.SlotOwner.Num());
		bool bAny = false;
		for (int32 Slot = 0; Slot < S.SlotOwner.Num(); ++Slot)
		{
			const FMusubiEntityRecord* R = Registry.Find(S.SlotOwner[Slot]);
			if (R && R->InstanceSlot == Slot)
			{
				IsmScratch[Slot] = FTransform(FRotator::ZeroRotator, R->RenderPos, FVector(S.Scale));
				bAny = true;
			}
			else IsmScratch[Slot] = FTransform(FRotator::ZeroRotator, FVector(0, 0, -1e7), FVector::ZeroVector);
		}
		if (bAny) Ism->BatchUpdateInstancesTransforms(0, IsmScratch, true, true, false);
	}
}

void UMusubiWorldComponent::BandwidthGuard(float DeltaTime)
{
	if (!bSubscribed) return;
	if (Stats.LateFramesPerSec > GuardLateFramesPerSec)
	{
		GuardBadSeconds += DeltaTime;
		if (GuardBadSeconds >= 3.f)
		{
			GuardBadSeconds = 0.f;
			const float Next = FMath::Max(MinRadius, EffectiveRadius * 0.8f);
			if (Next < EffectiveRadius) { EffectiveRadius = Next; SendSubscribe(true); }
		}
	}
	else
	{
		GuardBadSeconds = 0.f;
		const float Base = BaseRadius();
		if (EffectiveRadius < Base)
		{
			EffectiveRadius = FMath::Min(Base, EffectiveRadius * (1.f + 0.1f * DeltaTime));
			Stats.Radius = EffectiveRadius;
		}
	}
}

void UMusubiWorldComponent::RollStats(double Now)
{
	const double Window = FMath::Max(0.001, Now - StatsWindowStart);
	Stats.DeltasPerSec = (float)(DeltasThisSec / Window);
	Stats.SequencedPerSec = (float)(SequencedThisSec / Window);
	Stats.LateFramesPerSec = (float)(LateThisSec / Window);
	Stats.Entities = Registry.Num();
	Stats.TimeScale = TimeScale;
	Stats.Radius = EffectiveRadius;
	DeltasThisSec = SequencedThisSec = LateThisSec = 0;
	StatsWindowStart = Now;
	OnStats.Broadcast(Stats);
}

// ---- queries ----

AActor* UMusubiWorldComponent::FindEntityActor(const FString& EntityId) const
{
	FMusubiUuid U;
	if (!FMusubiUuid::Parse(EntityId, U)) return nullptr;
	const FMusubiEntityRecord* R = Registry.Find(U);
	return R && R->Actor.IsValid() ? R->Actor.Get() : nullptr;
}

bool UMusubiWorldComponent::GetEntityPosition(const FString& EntityId, FVector& OutPos) const
{
	FMusubiUuid U;
	if (!FMusubiUuid::Parse(EntityId, U)) return false;
	const FMusubiEntityRecord* R = Registry.Find(U);
	if (!R) return false;
	OutPos = R->Ring.Num() ? R->PosAt(PlaybackTick, MaxExtrapolateTicks) : R->RenderPos;
	return true;
}

TArray<FString> UMusubiWorldComponent::EntityIds() const
{
	TArray<FString> Out;
	Out.Reserve(Registry.Num());
	for (const auto& KV : Registry) Out.Add(KV.Key.ToString());
	return Out;
}

TArray<FString> UMusubiWorldComponent::EntitiesNear(FVector Center, float Radius) const
{
	TArray<TPair<double, FString>> Hits;
	const double R2 = (double)Radius * Radius;
	for (const auto& KV : Registry)
	{
		const double D2 = FVector::DistSquared2D(KV.Value.RenderPos, Center);
		if (D2 <= R2) Hits.Emplace(D2, KV.Key.ToString());
	}
	Hits.Sort([](const TPair<double, FString>& A, const TPair<double, FString>& B) { return A.Key < B.Key; });
	TArray<FString> Out;
	for (const auto& H : Hits) Out.Add(H.Value);
	return Out;
}
