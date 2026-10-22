#include "MusubiMassSinkComponent.h"
#include "MusubiMassTypes.h"
#include "MusubiWorldComponent.h"
#include "MusubiJson.h"
#include "MassEntitySubsystem.h"
#include "MassEntityManager.h"
#include "MassCommonFragments.h"
#include "Engine/World.h"

DEFINE_LOG_CATEGORY_STATIC(LogMusubiMass, Log, All);

UMusubiMassSinkComponent::UMusubiMassSinkComponent()
{
	PrimaryComponentTick.bCanEverTick = true;
	PrimaryComponentTick.bStartWithTickEnabled = true;
	PrimaryComponentTick.TickGroup = TG_PrePhysics;
}

FMassEntityManager* UMusubiMassSinkComponent::Manager() const
{
	UWorld* W = GetWorld();
	UMassEntitySubsystem* Sub = W ? W->GetSubsystem<UMassEntitySubsystem>() : nullptr;
	return Sub ? &Sub->GetMutableEntityManager() : nullptr;
}

bool UMusubiMassSinkComponent::EnsureArchetype(FMassEntityManager& M)
{
	if (Archetype.IsValid()) return true;
	// 5.3 takes (list, FName debug name), 5.5+ (list, FMassArchetypeCreationParams); the one-argument form is common.
	const UScriptStruct* Composition[] = { FTransformFragment::StaticStruct(), FMusubiEntityFragment::StaticStruct(), FMusubiEntityTag::StaticStruct() };
	Archetype = M.CreateArchetype(Composition);
	return Archetype.IsValid();
}

void UMusubiMassSinkComponent::BeginPlay()
{
	Super::BeginPlay();
	UMusubiWorldComponent* WC = World.Get();
	if (!WC && GetOwner()) WC = GetOwner()->FindComponentByClass<UMusubiWorldComponent>();
	if (!WC)
	{
		UE_LOG(LogMusubiMass, Warning, TEXT("%s: no UMusubiWorldComponent to mirror"), *GetPathName());
		return;
	}
	Bound = WC;
	AddTickPrerequisiteComponent(WC);
	SpawnedHandle = WC->OnRecordSpawned.AddUObject(this, &UMusubiMassSinkComponent::HandleSpawned);
	DespawnedHandle = WC->OnRecordDespawned.AddUObject(this, &UMusubiMassSinkComponent::HandleDespawned);
	ResetHandle = WC->OnRegistryReset.AddUObject(this, &UMusubiMassSinkComponent::HandleReset);
	// records that already exist (sink attached late)
	for (const auto& KV : WC->Entities()) HandleSpawned(KV.Value, true);
}

void UMusubiMassSinkComponent::EndPlay(const EEndPlayReason::Type Reason)
{
	if (UMusubiWorldComponent* WC = Bound.Get())
	{
		WC->OnRecordSpawned.Remove(SpawnedHandle);
		WC->OnRecordDespawned.Remove(DespawnedHandle);
		WC->OnRegistryReset.Remove(ResetHandle);
	}
	HandleReset();
	Bound.Reset();
	Super::EndPlay(Reason);
}

bool UMusubiMassSinkComponent::FindMassEntity(const FMusubiUuid& Id, FMassEntityHandle& Out) const
{
	const FTracked* T = Entities.Find(Id);
	if (!T) return false;
	Out = T->Entity;
	return true;
}

bool UMusubiMassSinkComponent::FindRecordId(FMassEntityHandle Entity, FMusubiUuid& Out) const
{
	const FMusubiUuid* Id = Reverse.Find(Entity);
	if (!Id) return false;
	Out = *Id;
	return true;
}

uint32 UMusubiMassSinkComponent::HashComponents(const FMusubiEntityRecord& R)
{
	// versions are monotonic per component (world component drops stale ones), so hashing them tracks any change
	uint32 H = 2166136261u;
	for (const auto& KV : R.Components)
	{
		H = HashCombine(H, GetTypeHash(KV.Key));
		H = HashCombine(H, GetTypeHash(KV.Value.Version));
	}
	return H;
}

void UMusubiMassSinkComponent::HandleSpawned(const FMusubiEntityRecord& R, bool bSpawn)
{
	if (R.Sink != EMusubiEntitySink::Mass)
	{
		// rule changed the sink on a re-sync: drop ours if we had one
		if (FTracked* Old = Entities.Find(R.Id))
		{
			if (FMassEntityManager* M = Manager()) Destroy(*M, R.Id, *Old);
			Entities.Remove(R.Id);
		}
		return;
	}
	FMassEntityManager* M = Manager();
	if (!M || !EnsureArchetype(*M))
	{
		UE_LOG(LogMusubiMass, Warning, TEXT("%s: no Mass entity manager in this world"), *GetPathName());
		return;
	}
	FTracked* T = Entities.Find(R.Id);
	if (T && !M->IsEntityValid(T->Entity))
	{
		Reverse.Remove(T->Entity);
		Entities.Remove(R.Id);
		T = nullptr;
	}
	const bool bNew = T == nullptr;
	if (bNew)
	{
		T = &Entities.Add(R.Id);
		T->Entity = M->CreateEntity(Archetype);
		Reverse.Add(T->Entity, R.Id);
		T->LastPos = R.RenderPos;
		if (FMusubiEntityFragment* F = M->GetFragmentDataPtr<FMusubiEntityFragment>(T->Entity)) F->SetUuid(R.Id);
	}
	WriteFragments(*M, R, *T, true);
	if (bNew) OnMassEntityCreated.Broadcast(*M, T->Entity, R);
}

void UMusubiMassSinkComponent::HandleDespawned(const FMusubiUuid& Id)
{
	FTracked* T = Entities.Find(Id);
	if (!T) return;
	if (FMassEntityManager* M = Manager()) Destroy(*M, Id, *T);
	else Reverse.Remove(T->Entity);
	Entities.Remove(Id);
}

void UMusubiMassSinkComponent::HandleReset()
{
	if (FMassEntityManager* M = Manager())
	{
		for (auto& KV : Entities) Destroy(*M, KV.Key, KV.Value);
	}
	Entities.Reset();
	Reverse.Reset();
}

void UMusubiMassSinkComponent::Destroy(FMassEntityManager& M, const FMusubiUuid& Id, FTracked& T)
{
	Reverse.Remove(T.Entity);
	if (!M.IsEntityValid(T.Entity)) return;
	OnMassEntityDestroying.Broadcast(M, T.Entity);
	// game-thread tick, outside Mass processing: immediate destroy is allowed (Defer() would wait for the next flush)
	M.DestroyEntity(T.Entity);
}

void UMusubiMassSinkComponent::WriteFragments(FMassEntityManager& M, const FMusubiEntityRecord& R, FTracked& T, bool bForce)
{
	if (!M.IsEntityValid(T.Entity)) return;
	const FVector Pos = R.RenderPos;
	double Yaw;
	const FMusubiEntityComponentState* YawC = YawComponent.IsEmpty() ? nullptr : R.FindComponent(YawComponent);
	if (YawC && YawC->Value.Value.IsValid() && YawC->Value.Value->TryGetNumber(Yaw)) T.Yaw = (float)Yaw;
	else if (FVector::DistSquared2D(Pos, T.LastPos) > 1.0) T.Yaw = (Pos - T.LastPos).Rotation().Yaw;
	const bool bMoved = bForce || !Pos.Equals(T.LastPos, 0.01);
	if (bMoved)
	{
		if (FTransformFragment* TF = M.GetFragmentDataPtr<FTransformFragment>(T.Entity))
			TF->SetTransform(FTransform(FRotator(0.f, T.Yaw, 0.f), Pos, FVector(Scale)));
		T.LastPos = Pos;
	}
	const uint32 H = HashComponents(R);
	if (bForce || H != T.ComponentsHash)
	{
		T.ComponentsHash = H;
		++T.Revision;
		if (FMusubiEntityFragment* F = M.GetFragmentDataPtr<FMusubiEntityFragment>(T.Entity))
		{
			F->LastTick = R.LastTick;
			F->ComponentsRevision = T.Revision;
			if (UMusubiWorldComponent* WC = Bound.Get()) F->bLocal = WC->GetLocalEntity() == R.Id.ToString();
		}
	}
}

void UMusubiMassSinkComponent::TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction)
{
	Super::TickComponent(DeltaTime, TickType, ThisTickFunction);
	UMusubiWorldComponent* WC = Bound.Get();
	FMassEntityManager* M = Manager();
	if (!WC || !M || Entities.Num() == 0) return;
	for (auto& KV : Entities)
	{
		const FMusubiEntityRecord* R = WC->FindRecord(KV.Key);
		if (!R) continue;
		WriteFragments(*M, *R, KV.Value, false);
	}
}
