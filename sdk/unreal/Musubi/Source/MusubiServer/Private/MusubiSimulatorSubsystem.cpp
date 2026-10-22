#include "MusubiSimulatorSubsystem.h"
#include "MusubiSimulatorSettings.h"
#include "MusubiAuthorityComponent.h"
#include "MusubiServerModule.h"
#include "MusubiJson.h"
#include "Containers/Ticker.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "GameFramework/Actor.h"
#include "Components/SceneComponent.h"
#include "Misc/Guid.h"
#include "Misc/CommandLine.h"
#include "Misc/Parse.h"

// ---- lifecycle ----

void UMusubiSimulatorSubsystem::Initialize(FSubsystemCollectionBase& Collection)
{
	Super::Initialize(Collection);
	LoadSettings();
	const UMusubiSimulatorSettings* S = GetDefault<UMusubiSimulatorSettings>();
	UGameInstance* GI = GetGameInstance();
	if (S && S->bAutoStart && GI && GI->IsDedicatedServerInstance() && !WorldName.IsEmpty()) Start();
}

void UMusubiSimulatorSubsystem::Deinitialize()
{
	Stop();
	Super::Deinitialize();
}

void UMusubiSimulatorSubsystem::LoadSettings()
{
	const UMusubiSimulatorSettings* S = GetDefault<UMusubiSimulatorSettings>();
	if (!S) return;
	ApiBase = S->ApiBase;
	ServerKey = S->ServerKey;
	WorldName = S->World;
	Capacity = FMath::Max(1, S->Capacity);
	Hz = FMath::Clamp(S->Hz, 1, 240);
	bDelta = S->bDeltaFrames;
	PositionEpsilon = S->PositionEpsilon;
	HeightComponent = S->HeightComponent;
	InitialCells = S->InitialCells;
	// box-level overrides: environment, then command line (-MusubiApi= -MusubiWorld= -MusubiCells=0,0;1,0)
	FString Env = FPlatformMisc::GetEnvironmentVariable(TEXT("MUSUBI_SERVER_KEY"));
	if (!Env.IsEmpty()) ServerKey = Env;
	Env = FPlatformMisc::GetEnvironmentVariable(TEXT("MUSUBI_API"));
	if (!Env.IsEmpty()) ApiBase = Env;
	Env = FPlatformMisc::GetEnvironmentVariable(TEXT("MUSUBI_WORLD"));
	if (!Env.IsEmpty()) WorldName = Env;
	FParse::Value(FCommandLine::Get(), TEXT("MusubiApi="), ApiBase);
	FParse::Value(FCommandLine::Get(), TEXT("MusubiWorld="), WorldName);
	FParse::Value(FCommandLine::Get(), TEXT("MusubiKey="), ServerKey);
	FString CellsArg;
	if (FParse::Value(FCommandLine::Get(), TEXT("MusubiCells="), CellsArg))
	{
		InitialCells.Reset();
		CellsArg.ParseIntoArray(InitialCells, TEXT(";"), true);
	}
}

void UMusubiSimulatorSubsystem::Configure(const FString& InApiBase, const FString& InServerKey, const FString& InWorld, int32 InCapacity, int32 InHz)
{
	ApiBase = InApiBase;
	ServerKey = InServerKey;
	WorldName = InWorld;
	Capacity = FMath::Max(1, InCapacity);
	Hz = FMath::Clamp(InHz, 1, 240);
}

static FMusubiUuid NewSimId()
{
	FMusubiUuid U;
	const FGuid G = FGuid::NewGuid();
	const uint32 Parts[4] = {G.A, G.B, G.C, G.D};
	FMemory::Memcpy(U.Bytes, Parts, 16);
	U.Bytes[6] = (U.Bytes[6] & 0x0f) | 0x40; // version 4
	U.Bytes[8] = (U.Bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
	return U;
}

void UMusubiSimulatorSubsystem::Start()
{
	if (bRunning) return;
	if (WorldName.IsEmpty() || ServerKey.IsEmpty())
	{
		UE_LOG(LogMusubiServer, Error, TEXT("simulator: World and ServerKey must be set (UMusubiSimulatorSettings / MUSUBI_WORLD, MUSUBI_SERVER_KEY)"));
		return;
	}
	bRunning = true;
	SimId = NewSimId();
	Link = MakeShared<FMusubiSimStream>();
	TWeakObjectPtr<UMusubiSimulatorSubsystem> Weak(this);
	Link->OnFrame.AddLambda([Weak](const FMusubiSimFrameTick& F) { if (Weak.IsValid()) Weak->Inbox.Add(F); });
	Link->OnFrameError.AddLambda([](uint64 Tick, const FString& Err) { UE_LOG(LogMusubiServer, Warning, TEXT("node frame error at tick %llu: %s"), Tick, *Err); });
	Link->OnStateChanged.AddLambda([Weak](EMusubiSimStreamState S) {
		if (!Weak.IsValid()) return;
		Weak->Stats.bConnected = S == EMusubiSimStreamState::Open;
		if (S == EMusubiSimStreamState::Open)
		{
			// a (re)connected stream starts with no leases: ask for the configured cells again
			TArray<FString> Want = Weak->InitialCells;
			for (const auto& KV : Weak->Cells) Want.AddUnique(CellKey(KV.Key));
			if (Want.Num()) Weak->LeaseCells(Want);
		}
	});
	FMusubiSimStream::FConfig Cfg;
	Cfg.bDelta = bDelta;
	Link->Connect(ApiBase, ServerKey, WorldName, SimId, Cfg);
	Accum = 0;
	StatsWindowStart = FPlatformTime::Seconds();
	TickerHandle = FTSTicker::GetCoreTicker().AddTicker(TEXT("MusubiSimulator"), 0.0f, [Weak](float Dt) {
		return Weak.IsValid() ? Weak->EngineTick(Dt) : false;
	});
	UE_LOG(LogMusubiServer, Log, TEXT("simulator %s started: world=%s hz=%d capacity=%d node=%s"), *SimId.ToString(), *WorldName, Hz, Capacity, *ApiBase);
}

void UMusubiSimulatorSubsystem::Stop()
{
	if (!bRunning) return;
	bRunning = false;
	FTSTicker::GetCoreTicker().RemoveTicker(TickerHandle);
	TickerHandle.Reset();
	if (Link.IsValid()) { Link->Close(); Link.Reset(); }
	TArray<FMusubiUuid> Ids;
	Entities.GetKeys(Ids);
	for (const FMusubiUuid& Id : Ids) RemoveEntity(Id, TEXT("stopped"));
	Cells.Reset();
	Inbox.Reset();
	Stats = FMusubiSimulatorStats();
}

// ---- loop ----

bool UMusubiSimulatorSubsystem::EngineTick(float DeltaSeconds)
{
	if (!bRunning) return false;
	if (Link.IsValid()) Link->Tick(DeltaSeconds);
	const double Step = 1.0 / (double)Hz;
	Accum += DeltaSeconds;
	int32 Steps = 0;
	while (Accum >= Step && Steps < 4)
	{
		Accum -= Step;
		SimTick();
		Steps++;
	}
	if (Accum > Step) Accum = Step; // fell too far behind: let tick_done carry the real tick count rather than bursting
	const double Now = FPlatformTime::Seconds();
	if (Now - StatsWindowStart >= 1.0) RollStats(Now);
	return true;
}

void UMusubiSimulatorSubsystem::SimTick()
{
	for (const FMusubiSimFrameTick& F : Inbox) ApplyFrame(F);
	Inbox.Reset();
	if (!Link.IsValid() || !Link->IsOpen()) return;
	CollectAndApply();
	TicksThisSec++;
	FMusubiSimCommand Done;
	Done.Tag = EMusubiSimCommandTag::TickDone;
	FMusubiSimCommandTickDone T;
	T.tick = LastTick;
	Done.TickDone = T;
	TWeakObjectPtr<UMusubiSimulatorSubsystem> Weak(this);
	SendCommand(MoveTemp(Done), [Weak](bool bOk, const TSharedPtr<FJsonObject>& R) {
		if (!bOk || !Weak.IsValid() || !R.IsValid()) return;
		const TArray<TSharedPtr<FJsonValue>>* Arr;
		if (!R->TryGetArrayField(TEXT("cells"), Arr)) return;
		TSet<FMusubiCellId> Live;
		for (const TSharedPtr<FJsonValue>& V : *Arr) { FMusubiCellId C; if (FMusubiCellId::FromJson(V, C)) Live.Add(C); }
		TArray<FMusubiCellId> Lost;
		for (const auto& KV : Weak->Cells) if (!Live.Contains(KV.Key)) Lost.Add(KV.Key);
		for (const FMusubiCellId& C : Lost)
		{
			UE_LOG(LogMusubiServer, Warning, TEXT("lease of cell %s not renewed"), *CellKey(C));
			FMusubiSimEvent E;
			E.Tag = EMusubiSimEventTag::CellRevoked;
			FMusubiSimEventCellRevoked Rv; Rv.cell = C; Rv.reason = TEXT("lease lost");
			E.CellRevoked = Rv;
			Weak->HandleEvent(E);
		}
		for (const FMusubiCellId& C : Live) if (!Weak->Cells.Contains(C))
		{
			FMusubiSimEvent E;
			E.Tag = EMusubiSimEventTag::CellAssigned;
			FMusubiSimEventCellAssigned As; As.cell = C; As.reason = TEXT("renew");
			E.CellAssigned = As;
			Weak->HandleEvent(E);
		}
	});
}

void UMusubiSimulatorSubsystem::ApplyFrame(const FMusubiSimFrameTick& F)
{
	FramesThisSec++;
	LastTick = FMath::Max(LastTick, F.tick);
	for (const FMusubiCellFrame& C : F.cells) ApplyCell(C);
	for (const FMusubiSimEvent& E : F.events) HandleEvent(E);
	Stats.Tick = (int64)LastTick;
}

void UMusubiSimulatorSubsystem::ApplyCell(const FMusubiCellFrame& C)
{
	FCell& Cell = Cells.FindOrAdd(C.cell);
	Cell.Id = C.cell;
	Cell.Tick = C.tick;
	const bool bFull = C.full.Get(false);
	TSet<FMusubiUuid> Seen;
	for (const FMusubiWorldEntity& E : C.owned) { UpsertEntity(E, C.cell, true, C.tick); Seen.Add(E.id); }
	for (const FMusubiWorldEntity& E : C.ghosts) { UpsertEntity(E, C.cell, false, C.tick); Seen.Add(E.id); }
	if (C.gone.IsSet()) for (const FMusubiUuid& Id : C.gone.GetValue()) RemoveEntity(Id, TEXT("gone"));
	if (bFull)
	{
		// a full view is the whole truth for this cell: whatever we had there and the node did not list is gone
		TArray<FMusubiUuid> Stale;
		for (const FMusubiUuid& Id : Cell.Owned) if (!Seen.Contains(Id)) Stale.Add(Id);
		for (const FMusubiUuid& Id : Cell.Ghosts) if (!Seen.Contains(Id)) Stale.Add(Id);
		for (const FMusubiUuid& Id : Stale) RemoveEntity(Id, TEXT("not in keyframe"));
	}
	if (C.time_scale.IsSet())
	{
		const float Scale = (float)C.time_scale.GetValue();
		if (!FMath::IsNearlyEqual(Scale, Cell.TimeScale, 0.001f))
		{
			Cell.TimeScale = Scale;
			for (const FMusubiUuid& Id : Cell.Owned)
				if (const FEntity* En = Entities.Find(Id); En && En->Actor.IsValid()) En->Actor->CustomTimeDilation = Scale;
			OnCellTimeScale.Broadcast(CellKey(C.cell), Scale);
		}
	}
}

// ---- entities ----

FVector UMusubiSimulatorSubsystem::WirePos(const FMusubiWorldEntity& E, const FVector& Fallback) const
{
	double Z = Fallback.Z;
	if (!HeightComponent.IsEmpty())
	{
		const FMusubiWorldComponent* C = E.components.FindByPredicate([&](const FMusubiWorldComponent& X) { return X.name == HeightComponent; });
		double V;
		if (C && C->value.Value.IsValid() && C->value.Value->TryGetNumber(V)) Z = V;
	}
	return FVector(E.x, E.y, Z);
}

AActor* UMusubiSimulatorSubsystem::SpawnActorFor(const FMusubiWorldEntity& E, bool bOwned, const FVector& At)
{
	UWorld* W = GetWorld();
	if (!W) return nullptr;
	const FTransform T(FRotator::ZeroRotator, At);
	if (SpawnEntityActor.IsBound())
		if (AActor* A = SpawnEntityActor.Execute(E, bOwned, T)) return A;
	const UMusubiSimulatorSettings* S = GetDefault<UMusubiSimulatorSettings>();
	UClass* Class = nullptr;
	if (S)
	{
		const TSoftClassPtr<AActor>& Soft = (!bOwned && !S->GhostActorClass.IsNull()) ? S->GhostActorClass : S->OwnedActorClass;
		Class = Soft.IsNull() ? nullptr : Soft.LoadSynchronous();
	}
	FActorSpawnParameters SP;
	SP.SpawnCollisionHandlingOverride = ESpawnActorCollisionHandlingMethod::AlwaysSpawn;
	SP.ObjectFlags |= RF_Transient;
	AActor* A = W->SpawnActor(Class ? Class : AActor::StaticClass(), &T, SP);
	if (A && !A->GetRootComponent())
	{
		USceneComponent* Root = NewObject<USceneComponent>(A, TEXT("Root"));
		A->SetRootComponent(Root);
		Root->RegisterComponent();
		A->SetActorLocation(At);
	}
	return A;
}

void UMusubiSimulatorSubsystem::UpsertEntity(const FMusubiWorldEntity& E, const FMusubiCellId& Cell, bool bOwned, uint64 Tick)
{
	FEntity* En = Entities.Find(E.id);
	FCell& C = Cells.FindOrAdd(Cell);
	C.Id = Cell;
	const bool bNew = En == nullptr || !En->Actor.IsValid();
	if (bNew)
	{
		const FVector At = WirePos(E, FVector::ZeroVector);
		AActor* A = SpawnActorFor(E, bOwned, At);
		if (!A) return;
		UMusubiAuthorityComponent* Auth = A->FindComponentByClass<UMusubiAuthorityComponent>();
		if (!Auth)
		{
			Auth = NewObject<UMusubiAuthorityComponent>(A, TEXT("MusubiAuthority"));
			A->AddInstanceComponent(Auth);
			Auth->RegisterComponent();
		}
		Auth->Bind(this, E.id, Cell, bOwned);
		Auth->LastSentPos = At;
		FEntity NewEn;
		NewEn.Actor = A;
		NewEn.Auth = Auth;
		NewEn.Cell = Cell;
		NewEn.bOwned = bOwned;
		En = &Entities.Add(E.id, NewEn);
		A->CustomTimeDilation = C.TimeScale;
	}
	else if (En->Cell != Cell)
	{
		if (FCell* Old = Cells.Find(En->Cell)) { Old->Owned.Remove(E.id); Old->Ghosts.Remove(E.id); }
		En->Cell = Cell;
		if (En->Auth.IsValid()) En->Auth->SetCell(Cell);
	}
	UMusubiAuthorityComponent* Auth = En->Auth.Get();
	AActor* A = En->Actor.Get();
	const bool bOwnershipChanged = En->bOwned != bOwned;
	En->bOwned = bOwned;
	if (bOwned) { C.Ghosts.Remove(E.id); C.Owned.Add(E.id); }
	else { C.Owned.Remove(E.id); C.Ghosts.Add(E.id); }
	if (Auth)
	{
		if (bOwnershipChanged) Auth->SetOwned(bOwned);
		Auth->ApplyFromNode(E);
		Auth->LastTick = Tick;
	}
	if (A && (!bOwned || bNew || bOwnershipChanged))
	{
		// ghosts follow the node; our own bodies move themselves (the node only echoes what we sent)
		const FVector P = WirePos(E, A->GetActorLocation());
		A->SetActorLocation(P, false, nullptr, ETeleportType::TeleportPhysics);
		if (Auth) Auth->LastSentPos = P;
	}
	if (bNew)
	{
		if (bOwned) OnEntitySpawned.Broadcast(E.id.ToString(), A);
		else OnGhostSpawned.Broadcast(E.id.ToString(), A);
	}
	else if (bOwnershipChanged && bOwned) OnEntitySpawned.Broadcast(E.id.ToString(), A);
}

void UMusubiSimulatorSubsystem::RemoveEntity(const FMusubiUuid& Id, const TCHAR* Why)
{
	FEntity En;
	if (!Entities.RemoveAndCopyValue(Id, En)) return;
	if (FCell* C = Cells.Find(En.Cell)) { C->Owned.Remove(Id); C->Ghosts.Remove(Id); }
	if (En.Actor.IsValid()) En.Actor->Destroy();
	if (En.bOwned) OnEntityDespawned.Broadcast(Id.ToString(), nullptr);
	UE_LOG(LogMusubiServer, Verbose, TEXT("entity %s removed: %s"), *Id.ToString(), Why);
}

void UMusubiSimulatorSubsystem::CollectAndApply()
{
	for (auto& KV : Entities)
	{
		FEntity& En = KV.Value;
		if (!En.bOwned || !En.Actor.IsValid() || !En.Auth.IsValid()) continue;
		UMusubiAuthorityComponent* Auth = En.Auth.Get();
		if (Auth->IsInHandover()) continue;
		const FVector P = En.Actor->GetActorLocation();
		const bool bMoved = FVector::Dist2D(P, Auth->LastSentPos) > PositionEpsilon;
		TSharedPtr<FJsonObject> Dirty = Auth->TakeDirty();
		if (!bMoved && !Dirty.IsValid()) continue;
		FMusubiSimCommand Cmd;
		Cmd.Tag = EMusubiSimCommandTag::Apply;
		FMusubiSimCommandApply Ap;
		Ap.entity = KV.Key.ToString();
		if (bMoved) { Ap.x = P.X; Ap.y = P.Y; Auth->LastSentPos = P; }
		if (Dirty.IsValid()) { FMusubiJson J; J.Value = MakeShared<FJsonValueObject>(Dirty); Ap.components = J; }
		Cmd.Apply = MoveTemp(Ap);
		SendCommand(MoveTemp(Cmd));
		AppliesThisSec++;
	}
}

void UMusubiSimulatorSubsystem::FlushNow() { CollectAndApply(); }

// ---- events ----

void UMusubiSimulatorSubsystem::HandleEvent(const FMusubiSimEvent& E)
{
	switch (E.Tag)
	{
	case EMusubiSimEventTag::CellAssigned:
		if (E.CellAssigned.IsSet())
		{
			FCell& C = Cells.FindOrAdd(E.CellAssigned->cell);
			C.Id = E.CellAssigned->cell;
			OnCellChanged.Broadcast(CellKey(C.Id), true);
		}
		break;
	case EMusubiSimEventTag::CellRevoked:
	case EMusubiSimEventTag::CellReleased:
	{
		const FMusubiCellId Cell = E.Tag == EMusubiSimEventTag::CellRevoked ? (E.CellRevoked.IsSet() ? E.CellRevoked->cell : FMusubiCellId())
																			  : (E.CellReleased.IsSet() ? E.CellReleased->cell : FMusubiCellId());
		FCell Gone;
		if (Cells.RemoveAndCopyValue(Cell, Gone))
		{
			TArray<FMusubiUuid> Ids = Gone.Owned.Array();
			Ids.Append(Gone.Ghosts.Array());
			for (const FMusubiUuid& Id : Ids) RemoveEntity(Id, TEXT("cell left"));
			OnCellChanged.Broadcast(CellKey(Cell), false);
		}
		break;
	}
	case EMusubiSimEventTag::CellHibernated:
		if (E.CellHibernated.IsSet()) if (FCell* C = Cells.Find(E.CellHibernated->cell)) C->bHibernated = true;
		break;
	case EMusubiSimEventTag::CellFull:
		if (E.CellFull.IsSet()) OnQueueEvent.Broadcast(CellKey(E.CellFull->cell), FString(), FString::Printf(TEXT("full:%u"), E.CellFull->soft_cap));
		break;
	case EMusubiSimEventTag::Queued:
		if (E.Queued.IsSet())
		{
			Stats.Queued++;
			OnQueueEvent.Broadcast(CellKey(E.Queued->cell), E.Queued->ticket.ToString(), E.Queued->eta_ticks.IsSet() ? FString::Printf(TEXT("queued:%llu/%llu"), (uint64)E.Queued->position, E.Queued->eta_ticks.GetValue()) : FString::Printf(TEXT("queued:%llu"), (uint64)E.Queued->position));
		}
		break;
	case EMusubiSimEventTag::QueueUpdate:
		if (E.QueueUpdate.IsSet()) OnQueueEvent.Broadcast(CellKey(E.QueueUpdate->cell), E.QueueUpdate->ticket.ToString(), FString::Printf(TEXT("update:%llu"), (uint64)E.QueueUpdate->position));
		break;
	case EMusubiSimEventTag::QueueAdmitted:
		if (E.QueueAdmitted.IsSet())
		{
			UpsertEntity(E.QueueAdmitted->entity, E.QueueAdmitted->cell, true, LastTick);
			OnQueueEvent.Broadcast(CellKey(E.QueueAdmitted->cell), E.QueueAdmitted->ticket.ToString(), TEXT("admitted"));
		}
		break;
	case EMusubiSimEventTag::QueueExpired:
		if (E.QueueExpired.IsSet()) OnQueueEvent.Broadcast(CellKey(E.QueueExpired->cell), E.QueueExpired->ticket.ToString(), TEXT("expired:") + E.QueueExpired->reason);
		break;
	case EMusubiSimEventTag::Decayed:
		if (E.Decayed.IsSet()) RemoveEntity(E.Decayed->entity, TEXT("decayed"));
		break;
	case EMusubiSimEventTag::TimeScale:
		if (E.TimeScale.IsSet())
		{
			FMusubiCellFrame F;
			F.cell = E.TimeScale->cell;
			F.tick = LastTick;
			F.time_scale = E.TimeScale->scale;
			ApplyCell(F);
		}
		break;
	case EMusubiSimEventTag::HandoverRequest:
		if (E.HandoverRequest.IsSet())
		{
			const FMusubiUuid Id = E.HandoverRequest->entity;
			UMusubiAuthorityComponent* Auth = FindAuthority(Id);
			const bool bAllow = !Auth || Auth->CanRelease();
			OnHandover.Broadcast(Id.ToString(), TEXT("request"), bAllow ? TEXT("ack") : TEXT("refused"));
			if (!bAllow) break;
			if (Auth)
			{
				// final authoritative state (incl. velocity) goes out before the ack so the receiver resumes from it
				Auth->CapturePhysicsIntoComponents();
				if (FEntity* En = Entities.Find(Id); En && En->Actor.IsValid())
				{
					FMusubiSimCommand Cmd;
					Cmd.Tag = EMusubiSimCommandTag::Apply;
					FMusubiSimCommandApply Ap;
					Ap.entity = Id.ToString();
					const FVector P = En->Actor->GetActorLocation();
					Ap.x = P.X; Ap.y = P.Y;
					Auth->LastSentPos = P;
					if (TSharedPtr<FJsonObject> D = Auth->TakeDirty()) { FMusubiJson J; J.Value = MakeShared<FJsonValueObject>(D); Ap.components = J; }
					Cmd.Apply = MoveTemp(Ap);
					SendCommand(MoveTemp(Cmd));
				}
			}
			FMusubiSimCommand Ack;
			Ack.Tag = EMusubiSimCommandTag::HandoverAck;
			FMusubiSimCommandHandoverAck H; H.entity = Id.ToString();
			Ack.HandoverAck = H;
			TWeakObjectPtr<UMusubiSimulatorSubsystem> Weak(this);
			SendCommand(MoveTemp(Ack), [Weak, Id](bool bOk, const TSharedPtr<FJsonObject>& R) {
				if (!Weak.IsValid()) return;
				FMusubiHandoverAckResult Res;
				const TSharedPtr<FJsonValue> Body = R.IsValid() ? R->TryGetField(TEXT("result")) : TSharedPtr<FJsonValue>();
				const bool bParsed = bOk && Body.IsValid() && FMusubiHandoverAckResult::FromJson(Body, Res);
				if (bParsed && Res.outcome.Tag == EMusubiHandoverOutcomeTag::Started)
				{
					if (UMusubiAuthorityComponent* A = Weak->FindAuthority(Id)) { A->SetInHandover(true); A->SetPhysicsEnabled(false); }
					Weak->Stats.HandoversOut++;
				}
				else if (!bOk)
				{
					FString Err; if (R.IsValid()) R->TryGetStringField(TEXT("error"), Err);
					UE_LOG(LogMusubiServer, Warning, TEXT("handover ack for %s failed: %s"), *Id.ToString(), *Err);
				}
			});
		}
		break;
	case EMusubiSimEventTag::HandoverStarted:
		if (E.HandoverStarted.IsSet())
		{
			if (UMusubiAuthorityComponent* A = FindAuthority(E.HandoverStarted->entity)) { A->SetInHandover(true); A->SetPhysicsEnabled(false); }
			OnHandover.Broadcast(E.HandoverStarted->entity.ToString(), TEXT("started"), E.HandoverStarted->to.ToString());
		}
		break;
	case EMusubiSimEventTag::HandoverCompleted:
		if (E.HandoverCompleted.IsSet())
		{
			// no longer ours; the next frame lists it as a ghost or gone, until then keep it kinematic
			if (FEntity* En = Entities.Find(E.HandoverCompleted->entity))
			{
				En->bOwned = false;
				if (FCell* C = Cells.Find(En->Cell)) { C->Owned.Remove(E.HandoverCompleted->entity); C->Ghosts.Add(E.HandoverCompleted->entity); }
				if (En->Auth.IsValid()) { En->Auth->SetInHandover(false); En->Auth->SetOwned(false); }
			}
			OnHandover.Broadcast(E.HandoverCompleted->entity.ToString(), TEXT("completed"), E.HandoverCompleted->to.ToString());
		}
		break;
	case EMusubiSimEventTag::HandoverCancelled:
		if (E.HandoverCancelled.IsSet())
		{
			if (UMusubiAuthorityComponent* A = FindAuthority(E.HandoverCancelled->entity)) { A->SetInHandover(false); if (A->IsOwned()) A->SetPhysicsEnabled(true); }
			OnHandover.Broadcast(E.HandoverCancelled->entity.ToString(), TEXT("cancelled"), E.HandoverCancelled->reason);
		}
		break;
	case EMusubiSimEventTag::HandoverArrived:
		if (E.HandoverArrived.IsSet())
		{
			const FMusubiSimEventHandoverArrived& A = E.HandoverArrived.GetValue();
			FMusubiCellId Cell;
			if (const FEntity* En = Entities.Find(A.entity)) Cell = En->Cell;
			else if (Cells.Num()) { for (const auto& KV : Cells) { Cell = KV.Key; break; } }
			UpsertEntity(A.state, Cell, true, LastTick);
			if (UMusubiAuthorityComponent* Auth = FindAuthority(A.entity))
			{
				Auth->SetInHandover(false);
				Auth->SetPhysicsEnabled(true);
				Auth->RestorePhysicsFromComponents();
				for (const FMusubiJson& In : A.buffered_inputs)
				{
					const TSharedPtr<FJsonObject>* O;
					TSharedPtr<FJsonValue> Input = In.Value.IsValid() && In.Value->TryGetObject(O) ? (*O)->TryGetField(TEXT("input")) : TSharedPtr<FJsonValue>();
					Auth->ApplyInput(Input.IsValid() ? MusubiJson::Serialize(Input) : In.ToText());
				}
			}
			Stats.HandoversIn++;
			OnHandover.Broadcast(A.entity.ToString(), TEXT("arrived"), A.from_node.ToString());
		}
		break;
	case EMusubiSimEventTag::Input:
		if (E.Input.IsSet())
			if (UMusubiAuthorityComponent* A = FindAuthority(E.Input->entity)) A->ApplyInput(E.Input->input.ToText());
		break;
	case EMusubiSimEventTag::Interaction:
		if (E.Interaction.IsSet())
			if (UMusubiAuthorityComponent* A = FindAuthority(E.Interaction->entity))
			{
				FString From;
				const TSharedPtr<FJsonObject>* O;
				if (E.Interaction->payload.Value.IsValid() && E.Interaction->payload.Value->TryGetObject(O)) (*O)->TryGetStringField(TEXT("from"), From);
				A->OnInteraction(E.Interaction->kind, E.Interaction->payload.ToText(), From);
			}
		break;
	case EMusubiSimEventTag::TxCommitted:
		if (E.TxCommitted.IsSet()) OnTxEvent.Broadcast(E.TxCommitted->tx.ToString(), E.TxCommitted->entity.ToString(), TEXT("committed"));
		break;
	case EMusubiSimEventTag::TxAborted:
		if (E.TxAborted.IsSet()) OnTxEvent.Broadcast(E.TxAborted->tx.ToString(), E.TxAborted->entity.ToString(), TEXT("aborted:") + E.TxAborted->reason);
		break;
	case EMusubiSimEventTag::CellReloaded:
	case EMusubiSimEventTag::CoLocate:
		break;
	case EMusubiSimEventTag::Unknown:
	{
		static bool bWarned = false;
		if (!bWarned) { bWarned = true; UE_LOG(LogMusubiServer, Warning, TEXT("node sent an event this module does not know; regenerate the wire layer")); }
		break;
	}
	}
	OnSimEvent.Broadcast(E);
}

// ---- commands ----

void UMusubiSimulatorSubsystem::SendCommand(FMusubiSimCommand&& Cmd, FMusubiSimReply OnReply)
{
	if (Link.IsValid()) Link->Send(Cmd, MoveTemp(OnReply));
	else if (OnReply) OnReply(false, nullptr);
}

bool UMusubiSimulatorSubsystem::ParseCell(const FString& S, FMusubiCellId& Out)
{
	TArray<FString> Parts;
	S.ParseIntoArray(Parts, TEXT(","), true);
	if (Parts.Num() < 2) return false;
	Out = FMusubiCellId();
	Out.x = FCString::Atoi(*Parts[0]);
	Out.y = FCString::Atoi(*Parts[1]);
	if (Parts.Num() > 2) Out.z = FCString::Atoi(*Parts[2]);
	if (Parts.Num() > 3) Out.level = (uint8)FCString::Atoi(*Parts[3]);
	return true;
}

void UMusubiSimulatorSubsystem::LeaseCells(const TArray<FString>& InCells)
{
	FMusubiSimCommand Cmd;
	Cmd.Tag = EMusubiSimCommandTag::Lease;
	FMusubiSimCommandLease L;
	for (const FString& S : InCells) { FMusubiCellId C; if (ParseCell(S, C)) L.cells.Add(C); }
	if (L.cells.Num() == 0) return;
	Cmd.Lease = MoveTemp(L);
	TWeakObjectPtr<UMusubiSimulatorSubsystem> Weak(this);
	SendCommand(MoveTemp(Cmd), [Weak](bool bOk, const TSharedPtr<FJsonObject>& R) {
		if (!Weak.IsValid() || !R.IsValid()) return;
		FString Redirect;
		if (R->TryGetStringField(TEXT("redirect"), Redirect)) { UE_LOG(LogMusubiServer, Warning, TEXT("lease: cells live on another node: %s"), *Redirect); return; }
		const TArray<TSharedPtr<FJsonValue>>* Arr;
		if (!bOk || !R->TryGetArrayField(TEXT("cells"), Arr)) return;
		for (const TSharedPtr<FJsonValue>& V : *Arr)
		{
			FMusubiCellId C;
			if (!FMusubiCellId::FromJson(V, C) || Weak->Cells.Contains(C)) continue;
			FMusubiSimEvent E;
			E.Tag = EMusubiSimEventTag::CellAssigned;
			FMusubiSimEventCellAssigned As; As.cell = C; As.reason = TEXT("lease");
			E.CellAssigned = As;
			Weak->HandleEvent(E);
		}
	});
}

void UMusubiSimulatorSubsystem::Spawn(double X, double Y, const FMusubiJson& Components, bool bPersistent, bool bGlobal, bool bQueue, TOptional<uint64> DecayTicks, FMusubiSimReply OnReply)
{
	FMusubiSimCommand Cmd;
	Cmd.Tag = EMusubiSimCommandTag::Spawn;
	FMusubiSimCommandSpawn S;
	S.x = X; S.y = Y; S.components = Components; S.persistent = bPersistent; S.global = bGlobal; S.queue = bQueue; S.decay_ticks = DecayTicks;
	Cmd.Spawn = MoveTemp(S);
	TWeakObjectPtr<UMusubiSimulatorSubsystem> Weak(this);
	SendCommand(MoveTemp(Cmd), [Weak, OnReply](bool bOk, const TSharedPtr<FJsonObject>& R) {
		if (Weak.IsValid() && bOk && R.IsValid())
		{
			// the entity is ours from this reply on; the next frame lists it under its cell and the actor appears then
			const TSharedPtr<FJsonObject>* Q;
			if (R->TryGetObjectField(TEXT("queued"), Q)) Weak->Stats.Queued++;
		}
		if (OnReply) OnReply(bOk, R);
	});
}

void UMusubiSimulatorSubsystem::SpawnEntity(double X, double Y, const FString& ComponentsJson, bool bPersistent, bool bGlobal)
{
	Spawn(X, Y, FMusubiJson::FromText(ComponentsJson), bPersistent, bGlobal, false, TOptional<uint64>());
}

void UMusubiSimulatorSubsystem::Despawn(const FMusubiUuid& Entity, FMusubiSimReply OnReply)
{
	FMusubiSimCommand Cmd;
	Cmd.Tag = EMusubiSimCommandTag::Despawn;
	FMusubiSimCommandDespawn D; D.entity = Entity.ToString();
	Cmd.Despawn = D;
	TWeakObjectPtr<UMusubiSimulatorSubsystem> Weak(this);
	SendCommand(MoveTemp(Cmd), [Weak, Entity, OnReply](bool bOk, const TSharedPtr<FJsonObject>& R) {
		if (bOk && Weak.IsValid()) Weak->RemoveEntity(Entity, TEXT("despawned"));
		if (OnReply) OnReply(bOk, R);
	});
}

void UMusubiSimulatorSubsystem::Attach(const FMusubiUuid& Entity, const FMusubiUuid& Root, bool bContained, FMusubiSimReply OnReply)
{
	FMusubiSimCommand Cmd;
	Cmd.Tag = EMusubiSimCommandTag::Attach;
	FMusubiSimCommandAttach A; A.entity = Entity.ToString(); A.root = Root.ToString(); A.contained = bContained;
	Cmd.Attach = A;
	SendCommand(MoveTemp(Cmd), MoveTemp(OnReply));
}

void UMusubiSimulatorSubsystem::Detach(const FMusubiUuid& Entity, FMusubiSimReply OnReply)
{
	FMusubiSimCommand Cmd;
	Cmd.Tag = EMusubiSimCommandTag::Detach;
	FMusubiSimCommandDetach D; D.entity = Entity.ToString();
	Cmd.Detach = D;
	SendCommand(MoveTemp(Cmd), MoveTemp(OnReply));
}

void UMusubiSimulatorSubsystem::Interact(const FMusubiUuid& Entity, const FString& Kind, const FMusubiJson& Payload, const FMusubiUuid& From, FMusubiSimReply OnReply)
{
	FMusubiSimCommand Cmd;
	Cmd.Tag = EMusubiSimCommandTag::Interact;
	FMusubiSimCommandInteract I;
	I.entity = Entity.ToString();
	I.kind = Kind;
	TSharedPtr<FJsonObject> P = MakeShared<FJsonObject>();
	const TSharedPtr<FJsonObject>* Given;
	if (Payload.Value.IsValid() && Payload.Value->TryGetObject(Given)) P->Values = (*Given)->Values;
	P->SetStringField(TEXT("from"), From.ToString());
	FMusubiJson J; J.Value = MakeShared<FJsonValueObject>(P);
	I.payload = J;
	I.tick = LastTick;
	if (const FEntity* En = Entities.Find(From); En && En->Actor.IsValid())
	{
		const FVector L = En->Actor->GetActorLocation();
		I.x = L.X; I.y = L.Y;
	}
	Cmd.Interact = MoveTemp(I);
	SendCommand(MoveTemp(Cmd), MoveTemp(OnReply));
}

void UMusubiSimulatorSubsystem::Colocate(const FMusubiUuid& Entity, const FMusubiUuid& With, uint64 TtlTicks, FMusubiSimReply OnReply)
{
	FMusubiSimCommand Cmd;
	Cmd.Tag = EMusubiSimCommandTag::Colocate;
	FMusubiSimCommandColocate C; C.entity = Entity.ToString(); C.with = With.ToString(); C.ttl_ticks = TtlTicks;
	Cmd.Colocate = C;
	SendCommand(MoveTemp(Cmd), MoveTemp(OnReply));
}

void UMusubiSimulatorSubsystem::Rewind(const FMusubiUuid& Entity, uint64 Tick, TOptional<FVector2D> At, TOptional<double> Tolerance, FMusubiSimReply OnReply)
{
	FMusubiSimCommand Cmd;
	Cmd.Tag = EMusubiSimCommandTag::Rewind;
	FMusubiSimCommandRewind R; R.entity = Entity.ToString(); R.tick = Tick;
	if (At.IsSet()) { R.x = At->X; R.y = At->Y; }
	R.tolerance = Tolerance;
	Cmd.Rewind = R;
	SendCommand(MoveTemp(Cmd), MoveTemp(OnReply));
}

// ---- queries / stats ----

UMusubiAuthorityComponent* UMusubiSimulatorSubsystem::FindAuthority(const FMusubiUuid& Id) const
{
	const FEntity* En = Entities.Find(Id);
	return En && En->Auth.IsValid() ? En->Auth.Get() : nullptr;
}

AActor* UMusubiSimulatorSubsystem::FindEntityActor(const FString& EntityId) const
{
	FMusubiUuid U;
	if (!FMusubiUuid::Parse(EntityId, U)) return nullptr;
	const FEntity* En = Entities.Find(U);
	return En && En->Actor.IsValid() ? En->Actor.Get() : nullptr;
}

TArray<AActor*> UMusubiSimulatorSubsystem::OwnedActors() const
{
	TArray<AActor*> Out;
	for (const auto& KV : Entities) if (KV.Value.bOwned && KV.Value.Actor.IsValid()) Out.Add(KV.Value.Actor.Get());
	return Out;
}

TArray<FString> UMusubiSimulatorSubsystem::LeasedCells() const
{
	TArray<FString> Out;
	for (const auto& KV : Cells) Out.Add(CellKey(KV.Key));
	return Out;
}

float UMusubiSimulatorSubsystem::CellTimeScale(const FString& CellText) const
{
	FMusubiCellId C;
	if (!ParseCell(CellText, C)) return 1.f;
	const FCell* Cell = Cells.Find(C);
	return Cell ? Cell->TimeScale : 1.f;
}

void UMusubiSimulatorSubsystem::RollStats(double Now)
{
	const double W = FMath::Max(0.001, Now - StatsWindowStart);
	Stats.Cells = Cells.Num();
	int32 Owned = 0, Ghosts = 0;
	float Slowest = 1.f;
	for (const auto& KV : Cells) { Owned += KV.Value.Owned.Num(); Ghosts += KV.Value.Ghosts.Num(); Slowest = FMath::Min(Slowest, KV.Value.TimeScale); }
	Stats.Owned = Owned;
	Stats.Ghosts = Ghosts;
	Stats.SlowestTimeScale = Slowest;
	Stats.FramesPerSec = (float)(FramesThisSec / W);
	Stats.TicksPerSec = (float)(TicksThisSec / W);
	Stats.AppliesPerSec = (float)(AppliesThisSec / W);
	FramesThisSec = TicksThisSec = AppliesThisSec = 0;
	StatsWindowStart = Now;
	OnStats.Broadcast(Stats);
}
