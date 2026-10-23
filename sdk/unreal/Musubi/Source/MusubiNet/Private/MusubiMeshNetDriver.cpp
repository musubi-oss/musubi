#include "MusubiMeshNetDriver.h"
#include "MusubiSimulatorSubsystem.h"
#include "MusubiAuthorityComponent.h"
#include "MusubiJson.h"
#include "Engine/Engine.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "GameFramework/Actor.h"
#include "Subsystems/SubsystemCollection.h"
#include "Serialization/BitWriter.h"
#include "Serialization/BitReader.h"
#include "Misc/Base64.h"
#include "UObject/UnrealType.h"
#include "UObject/Package.h"

DEFINE_LOG_CATEGORY_STATIC(LogMusubiMesh, Log, All);

const FName UMusubiMeshNetDriver::DriverName(TEXT("MusubiMesh"));
const FName UMusubiMeshNetDriver::DefinitionName(TEXT("MusubiMeshNetDriver"));

bool UMusubiMeshNetDriver::InitConnect(FNetworkNotify*, const FURL&, FString& Error)
{
	Error = TEXT("MusubiMesh is a server-side driver");
	return false;
}

bool UMusubiMeshNetDriver::InitListen(FNetworkNotify* InNotify, FURL& LocalURL, bool bReuseAddressAndPort, FString& Error)
{
	if (!InitBase(false, InNotify, LocalURL, bReuseAddressAndPort, Error)) return false;
	ServerConnection = nullptr; // no connections: ticks replicate nothing, only RPC routing happens
	return true;
}

static UMusubiAuthorityComponent* AuthorityOf(const AActor* Actor)
{
	return Actor ? Actor->FindComponentByClass<UMusubiAuthorityComponent>() : nullptr;
}

bool UMusubiMeshNetDriver::ShouldReplicateFunction(AActor* Actor, UFunction* Function) const
{
	if (!Actor || !Function || !(Function->FunctionFlags & FUNC_Net)) return false;
	const UMusubiAuthorityComponent* Auth = AuthorityOf(Actor);
	if (!Auth) return false;
	// ghosts: everything the game code tries to call on them belongs to the owner; owned: only what the owner's own
	// engine driver cannot deliver is a mesh call — nothing today (Client RPCs for remote controllers are 9e)
	return Auth->IsGhost();
}

void UMusubiMeshNetDriver::ProcessRemoteFunction(AActor* Actor, UFunction* Function, void* Parms, FOutParmRec*, FFrame*, UObject* SubObject)
{
	UMusubiAuthorityComponent* Auth = AuthorityOf(Actor);
	UGameInstance* GI = GetWorld() ? GetWorld()->GetGameInstance() : nullptr;
	UMusubiSimulatorSubsystem* Sim = GI ? GI->GetSubsystem<UMusubiSimulatorSubsystem>() : nullptr;
	if (!Auth || !Sim || !Function) { ++Dropped; return; }
	if (SubObject && SubObject != Actor)
	{
		// component RPCs: addressed by the component's name on the actor
	}
	TArray<uint8> Bytes;
	int64 Bits = 0;
	if (!EncodeParams(Function, Parms, Bytes, Bits)) { ++Dropped; return; }
	TSharedPtr<FJsonObject> P = MakeShared<FJsonObject>();
	P->SetStringField(TEXT("f"), Function->GetPathName());
	P->SetStringField(TEXT("c"), Actor->GetClass()->GetPathName());
	if (SubObject && SubObject != Actor) P->SetStringField(TEXT("sub"), SubObject->GetName());
	P->SetStringField(TEXT("b"), FBase64::Encode(Bytes.GetData(), Bytes.Num()));
	P->SetNumberField(TEXT("bits"), (double)Bits);
	FMusubiUuid Entity;
	if (!FMusubiUuid::Parse(Auth->GetEntityId(), Entity)) { ++Dropped; return; }
	FMusubiJson J;
	J.Value = MakeShared<FJsonValueObject>(P);
	Sim->Interact(Entity, TEXT("rpc"), J, Entity);
	++Sent;
}

bool UMusubiMeshNetDriver::EncodeParams(UFunction* Function, void* Parms, TArray<uint8>& OutBytes, int64& OutBits)
{
	FBitWriter W(0, true);
	for (TFieldIterator<FProperty> It(Function); It && (It->PropertyFlags & (CPF_Parm | CPF_ReturnParm)) == CPF_Parm; ++It)
	{
		for (int32 i = 0; i < It->ArrayDim; ++i)
			It->NetSerializeItem(W, nullptr, It->ContainerPtrToValuePtr<void>(Parms, i));
	}
	if (W.IsError()) return false;
	OutBits = W.GetNumBits();
	OutBytes.SetNumUninitialized(W.GetNumBytes());
	FMemory::Memcpy(OutBytes.GetData(), W.GetData(), W.GetNumBytes());
	return true;
}

bool UMusubiMeshNetDriver::DecodeAndCall(UObject* Target, UFunction* Function, const TArray<uint8>& Bytes, int64 Bits)
{
	if (!Target || !Function || Bits < 0 || (int64)Bytes.Num() * 8 < Bits) return false;
	FBitReader R(Bytes.GetData(), Bits);
	uint8* Parms = Function->ParmsSize > 0 ? (uint8*)FMemory_Alloca_Aligned(Function->ParmsSize, 16) : nullptr;
	if (Parms) FMemory::Memzero(Parms, Function->ParmsSize);
	for (TFieldIterator<FProperty> It(Function); It && (It->PropertyFlags & (CPF_Parm | CPF_ReturnParm)) == CPF_Parm; ++It)
	{
		It->InitializeValue_InContainer(Parms);
		for (int32 i = 0; i < It->ArrayDim; ++i)
			It->NetSerializeItem(R, nullptr, It->ContainerPtrToValuePtr<void>(Parms, i));
	}
	const bool bOk = !R.IsError();
	if (bOk) Target->ProcessEvent(Function, Parms);
	for (TFieldIterator<FProperty> It(Function); It && (It->PropertyFlags & (CPF_Parm | CPF_ReturnParm)) == CPF_Parm; ++It)
		It->DestroyValue_InContainer(Parms);
	return bOk;
}

// ---------------------------------------------------------------------------------------------------------------

bool UMusubiMeshRpcSubsystem::ShouldCreateSubsystem(UObject* Outer) const
{
	return Super::ShouldCreateSubsystem(Outer) && IsRunningDedicatedServer();
}

void UMusubiMeshRpcSubsystem::Initialize(FSubsystemCollectionBase& Collection)
{
	Super::Initialize(Collection);
	UMusubiSimulatorSubsystem* SimSub = Collection.InitializeDependency<UMusubiSimulatorSubsystem>();
	if (!SimSub) return;
	Sim = SimSub;
	StartedHandle = SimSub->OnStarted.AddUObject(this, &UMusubiMeshRpcSubsystem::OnStarted);
	StoppedHandle = SimSub->OnStopped.AddUObject(this, &UMusubiMeshRpcSubsystem::OnStopped);
	RpcHandle = SimSub->OnRpc.AddUObject(this, &UMusubiMeshRpcSubsystem::OnRpc);
	if (SimSub->IsRunning()) OnStarted();
}

void UMusubiMeshRpcSubsystem::Deinitialize()
{
	OnStopped();
	if (UMusubiSimulatorSubsystem* S = Sim.Get())
	{
		S->OnStarted.Remove(StartedHandle);
		S->OnStopped.Remove(StoppedHandle);
		S->OnRpc.Remove(RpcHandle);
	}
	Super::Deinitialize();
}

void UMusubiMeshRpcSubsystem::OnStarted()
{
	UWorld* W = GetGameInstance() ? GetGameInstance()->GetWorld() : nullptr;
	if (!W || !GEngine || GEngine->FindNamedNetDriver(W, UMusubiMeshNetDriver::DriverName)) return;
	if (!GEngine->CreateNamedNetDriver(W, UMusubiMeshNetDriver::DriverName, UMusubiMeshNetDriver::DefinitionName))
	{
		UE_LOG(LogMusubiMesh, Error, TEXT("no NetDriverDefinitions entry %s — add DefName=\"MusubiMeshNetDriver\",DriverClassName=\"/Script/MusubiNet.MusubiMeshNetDriver\" to DefaultEngine.ini"), *UMusubiMeshNetDriver::DefinitionName.ToString());
		return;
	}
	UNetDriver* D = GEngine->FindNamedNetDriver(W, UMusubiMeshNetDriver::DriverName);
	if (!D) return;
	D->SetWorld(W);
	FURL URL;
	FString Error;
	if (!D->InitListen(nullptr, URL, false, Error))
	{
		UE_LOG(LogMusubiMesh, Error, TEXT("MusubiMesh InitListen: %s"), *Error);
		GEngine->DestroyNamedNetDriver(W, UMusubiMeshNetDriver::DriverName);
	}
}

void UMusubiMeshRpcSubsystem::OnStopped()
{
	UWorld* W = GetGameInstance() ? GetGameInstance()->GetWorld() : nullptr;
	if (W && GEngine && GEngine->FindNamedNetDriver(W, UMusubiMeshNetDriver::DriverName))
		GEngine->DestroyNamedNetDriver(W, UMusubiMeshNetDriver::DriverName);
}

void UMusubiMeshRpcSubsystem::OnRpc(AActor* Actor, const FMusubiJson& Payload)
{
	const TSharedPtr<FJsonObject>* O = nullptr;
	if (!Actor || !Payload.Value.IsValid() || !Payload.Value->TryGetObject(O) || !O) { ++Rejected; return; }
	const FString FnPath = (*O)->GetStringField(TEXT("f"));
	UFunction* Fn = FindObject<UFunction>(nullptr, *FnPath);
	UObject* Target = Actor;
	FString Sub;
	if ((*O)->TryGetStringField(TEXT("sub"), Sub))
	{
		Target = nullptr;
		for (UActorComponent* C : Actor->GetComponents()) if (C && C->GetName() == Sub) { Target = C; break; }
	}
	TArray<uint8> Bytes;
	if (!Fn || !Target || !Target->IsA(Fn->GetOwnerClass()) || !FBase64::Decode((*O)->GetStringField(TEXT("b")), Bytes))
	{
		++Rejected;
		UE_LOG(LogMusubiMesh, Warning, TEXT("rpc %s on %s rejected"), *FnPath, *Actor->GetName());
		return;
	}
	if (UMusubiMeshNetDriver::DecodeAndCall(Target, Fn, Bytes, (int64)(*O)->GetNumberField(TEXT("bits")))) ++Executed;
	else ++Rejected;
}
