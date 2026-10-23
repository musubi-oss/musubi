#include "MusubiDemoContentCommandlet.h"
#include "MusubiThirdPersonCharacter.h"
#include "MusubiThirdPersonInventory.h"
#include "MusubiAuthorityComponent.h"
#include "Engine/Blueprint.h"
#include "EdGraph/EdGraph.h"
#include "EdGraphSchema_K2.h"
#include "K2Node_AddDelegate.h"
#include "K2Node_CallFunction.h"
#include "K2Node_CustomEvent.h"
#include "K2Node_Event.h"
#include "K2Node_IfThenElse.h"
#include "K2Node_VariableGet.h"
#include "Kismet/KismetStringLibrary.h"
#include "Kismet/KismetSystemLibrary.h"
#include "Kismet2/BlueprintEditorUtils.h"
#include "Kismet2/CompilerResultsLog.h"
#include "Kismet2/KismetEditorUtilities.h"
#include "Misc/PackageName.h"
#include "UObject/Package.h"
#include "UObject/SavePackage.h"

DEFINE_LOG_CATEGORY_STATIC(LogMusubiDemo, Log, All);

static const FString Tag = TEXT("musubi-demo");

namespace
{
template<class T> T* Place(UEdGraph* G, int32 X, int32 Y, TFunctionRef<void(T*)> Setup)
{
	FGraphNodeCreator<T> C(*G);
	T* N = C.CreateNode(false);
	N->NodePosX = X; N->NodePosY = Y;
	N->NodeComment = Tag; N->bCommentBubbleVisible = false;
	Setup(N);
	C.Finalize();
	return N;
}

UK2Node_CallFunction* Call(UEdGraph* G, int32 X, int32 Y, UClass* Cls, FName Fn)
{
	UFunction* F = Cls->FindFunctionByName(Fn);
	checkf(F, TEXT("%s::%s"), *Cls->GetName(), *Fn.ToString());
	return Place<UK2Node_CallFunction>(G, X, Y, [F](UK2Node_CallFunction* N) { N->SetFromFunction(F); });
}

// Cls == nullptr: a member of the Blueprint's own class (inherited C++ UPROPERTY included) — no target pin
UK2Node_VariableGet* Get(UEdGraph* G, int32 X, int32 Y, UClass* Cls, FName Var)
{
	return Place<UK2Node_VariableGet>(G, X, Y, [&](UK2Node_VariableGet* N) { if (Cls) N->VariableReference.SetExternalMember(Var, Cls); else N->VariableReference.SetSelfMember(Var); });
}

bool Link(UEdGraphPin* A, UEdGraphPin* B)
{
	check(A && B);
	const bool bOk = GetDefault<UEdGraphSchema_K2>()->TryCreateConnection(A, B);
	if (!bOk) UE_LOG(LogMusubiDemo, Error, TEXT("cannot link %s.%s -> %s.%s"), *A->GetOwningNode()->GetName(), *A->PinName.ToString(), *B->GetOwningNode()->GetName(), *B->PinName.ToString());
	return bOk;
}

UEdGraphPin* Pin(UEdGraphNode* N, const TCHAR* Name) { return N->FindPinChecked(FName(Name)); }
UEdGraphPin* Self(UEdGraphNode* N) { return N->FindPinChecked(UEdGraphSchema_K2::PN_Self); }
void Default(UEdGraphNode* N, const TCHAR* Name, const TCHAR* Value) { GetDefault<UEdGraphSchema_K2>()->TrySetDefaultValue(*Pin(N, Name), Value); }
}

UMusubiDemoContentCommandlet::UMusubiDemoContentCommandlet()
{
	IsClient = false; IsServer = false; IsEditor = true; LogToConsole = true;
}

int32 UMusubiDemoContentCommandlet::Main(const FString& Params)
{
	const FString Path = TEXT("/Game/ThirdPerson/Blueprints/BP_ThirdPersonCharacter.BP_ThirdPersonCharacter");
	UBlueprint* BP = LoadObject<UBlueprint>(nullptr, *Path);
	if (!BP) { UE_LOG(LogMusubiDemo, Error, TEXT("%s not found (run tools/demo-content.sh first)"), *Path); return 1; }
	if (!BP->ParentClass || !BP->ParentClass->IsChildOf(AMusubiThirdPersonCharacter::StaticClass()))
	{
		UE_LOG(LogMusubiDemo, Error, TEXT("%s parent is %s, expected AMusubiThirdPersonCharacter (CoreRedirects?)"), *Path, *GetNameSafe(BP->ParentClass));
		return 1;
	}
	UEdGraph* G = FBlueprintEditorUtils::FindEventGraph(BP);
	if (!G) { UE_LOG(LogMusubiDemo, Error, TEXT("no event graph")); return 1; }

	// re-runnable: drop what a previous run placed
	int32 Removed = 0;
	for (UEdGraphNode* N : TArray<UEdGraphNode*>(G->Nodes))
		if (N && N->NodeComment == Tag) { FBlueprintEditorUtils::RemoveNode(BP, N, true); ++Removed; }

	UClass* Chr = AMusubiThirdPersonCharacter::StaticClass();
	UClass* Inv = UMusubiThirdPersonInventory::StaticClass();
	UClass* Sys = UKismetSystemLibrary::StaticClass();
	UClass* Str = UKismetStringLibrary::StaticClass();
	bool bOk = true;

	// BeginPlay (reuse the template's override if there is one)
	UK2Node_Event* Begin = nullptr;
	for (UEdGraphNode* N : G->Nodes)
		if (UK2Node_Event* E = Cast<UK2Node_Event>(N))
			if (E->bOverrideFunction && E->EventReference.GetMemberName() == TEXT("ReceiveBeginPlay")) Begin = E;
	TArray<UEdGraphPin*> OldThen;
	if (Begin) { OldThen = Begin->GetThenPin()->LinkedTo; Begin->GetThenPin()->BreakAllPinLinks(); }
	else Begin = Place<UK2Node_Event>(G, 0, 1200, [](UK2Node_Event* N) { N->EventReference.SetExternalMember(TEXT("ReceiveBeginPlay"), AActor::StaticClass()); N->bOverrideFunction = true; });

	// BeginPlay -> Bind Event to Inventory.OnInventoryChanged -> custom event -> PrintString("musubi-demo inventory items=N gold=G")
	// (BeginPlay runs inside SpawnActor, before the simulator binds the authority component: ownership is not known yet there,
	// so everything that depends on it hangs off Authority.OnAuthorityChanged below, not off a Branch at BeginPlay)
	UK2Node_VariableGet* InvGet = Get(G, 0, 1500, nullptr, TEXT("Inventory"));
	FMulticastDelegateProperty* Dlg = CastField<FMulticastDelegateProperty>(Inv->FindPropertyByName(TEXT("OnInventoryChanged")));
	check(Dlg && Dlg->SignatureFunction);
	UK2Node_AddDelegate* Bind = Place<UK2Node_AddDelegate>(G, 300, 1200, [&](UK2Node_AddDelegate* N) { N->SetFromProperty(Dlg, false, Inv); });
	UK2Node_CustomEvent* Changed = UK2Node_CustomEvent::CreateFromFunction(FVector2D(0, 1800), G, TEXT("MusubiDemoInventoryChanged"), Dlg->SignatureFunction, false);
	Changed->NodeComment = Tag; Changed->bCommentBubbleVisible = false;
	UK2Node_CallFunction* Items = Call(G, 100, 2000, Inv, GET_FUNCTION_NAME_CHECKED(UMusubiThirdPersonInventory, NumItems));
	UK2Node_VariableGet* Gold = Get(G, 100, 2150, Inv, TEXT("Gold"));
	UK2Node_CallFunction* S1 = Call(G, 350, 2000, Str, GET_FUNCTION_NAME_CHECKED(UKismetStringLibrary, BuildString_Int));
	UK2Node_CallFunction* S2 = Call(G, 600, 2000, Str, GET_FUNCTION_NAME_CHECKED(UKismetStringLibrary, BuildString_Int));
	UK2Node_CallFunction* Print2 = Call(G, 900, 1800, Sys, GET_FUNCTION_NAME_CHECKED(UKismetSystemLibrary, PrintString));
	Default(S1, TEXT("Prefix"), TEXT("musubi-demo inventory items="));
	Default(S2, TEXT("Prefix"), TEXT(" gold="));
	Default(Print2, TEXT("Duration"), TEXT("5.0"));
	bOk &= Link(Begin->GetThenPin(), Bind->GetExecPin());
	bOk &= Link(InvGet->GetValuePin(), Bind->FindPinChecked(UEdGraphSchema_K2::PN_Self));
	bOk &= Link(Changed->FindPinChecked(UK2Node_Event::DelegateOutputName), Bind->GetDelegatePin());
	bOk &= Link(InvGet->GetValuePin(), Self(Items));
	bOk &= Link(InvGet->GetValuePin(), Self(Gold));
	bOk &= Link(Items->GetReturnValuePin(), Pin(S1, TEXT("InInt")));
	bOk &= Link(S1->GetReturnValuePin(), Pin(S2, TEXT("AppendTo")));
	bOk &= Link(Gold->GetValuePin(), Pin(S2, TEXT("InInt")));
	bOk &= Link(S2->GetReturnValuePin(), Pin(Print2, TEXT("InString")));
	bOk &= Link(Changed->GetThenPin(), Print2->GetExecPin());

	// ... -> Bind Event to Authority.OnAuthorityChanged -> custom event(bOwned) -> Branch -> PrintString("musubi-demo owned entity <id>")
	UClass* AuthCls = UMusubiAuthorityComponent::StaticClass();
	UK2Node_VariableGet* AuthGet = Get(G, 300, 1500, nullptr, TEXT("Authority"));
	FMulticastDelegateProperty* ADlg = CastField<FMulticastDelegateProperty>(AuthCls->FindPropertyByName(TEXT("OnAuthorityChanged")));
	check(ADlg && ADlg->SignatureFunction);
	UK2Node_AddDelegate* ABind = Place<UK2Node_AddDelegate>(G, 600, 1200, [&](UK2Node_AddDelegate* N) { N->SetFromProperty(ADlg, false, AuthCls); });
	UK2Node_CustomEvent* AChanged = UK2Node_CustomEvent::CreateFromFunction(FVector2D(0, 2300), G, TEXT("MusubiDemoAuthorityChanged"), ADlg->SignatureFunction, false);
	AChanged->NodeComment = Tag; AChanged->bCommentBubbleVisible = false;
	UK2Node_IfThenElse* Branch = Place<UK2Node_IfThenElse>(G, 300, 2300, [](UK2Node_IfThenElse*) {});
	UK2Node_CallFunction* Id = Call(G, 300, 2450, Chr, GET_FUNCTION_NAME_CHECKED(AMusubiThirdPersonCharacter, GetEntityId));
	UK2Node_CallFunction* Cat = Call(G, 500, 2450, Str, GET_FUNCTION_NAME_CHECKED(UKismetStringLibrary, Concat_StrStr));
	UK2Node_CallFunction* Print = Call(G, 700, 2300, Sys, GET_FUNCTION_NAME_CHECKED(UKismetSystemLibrary, PrintString));
	Default(Cat, TEXT("A"), TEXT("musubi-demo owned entity "));
	Default(Print, TEXT("Duration"), TEXT("10.0"));
	bOk &= Link(Bind->GetThenPin(), ABind->GetExecPin());
	bOk &= Link(AuthGet->GetValuePin(), ABind->FindPinChecked(UEdGraphSchema_K2::PN_Self));
	bOk &= Link(AChanged->FindPinChecked(UK2Node_Event::DelegateOutputName), ABind->GetDelegatePin());
	bOk &= Link(AChanged->GetThenPin(), Branch->GetExecPin());
	bOk &= Link(AChanged->FindPinChecked(TEXT("bOwned")), Branch->GetConditionPin());
	bOk &= Link(Branch->GetThenPin(), Print->GetExecPin());
	bOk &= Link(Id->GetReturnValuePin(), Pin(Cat, TEXT("B")));
	bOk &= Link(Cat->GetReturnValuePin(), Pin(Print, TEXT("InString")));

	// ... -> Set Timer by Event (2 s, looping) -> custom event -> Branch(IsEntityOwned) -> AddGold(10), AddItem(1, 1)
	UK2Node_CallFunction* Timer = Call(G, 900, 1200, Sys, GET_FUNCTION_NAME_CHECKED(UKismetSystemLibrary, K2_SetTimerDelegate));
	UK2Node_CallFunction* Owned = Call(G, 100, 2650, Chr, GET_FUNCTION_NAME_CHECKED(AMusubiThirdPersonCharacter, IsEntityOwned));
	UK2Node_CustomEvent* Tick = Place<UK2Node_CustomEvent>(G, 0, 2500, [](UK2Node_CustomEvent* N) { N->CustomFunctionName = TEXT("MusubiDemoPickup"); });
	UK2Node_IfThenElse* Branch2 = Place<UK2Node_IfThenElse>(G, 300, 2500, [](UK2Node_IfThenElse*) {});
	UK2Node_CallFunction* AddGold = Call(G, 600, 2500, Inv, GET_FUNCTION_NAME_CHECKED(UMusubiThirdPersonInventory, AddGold));
	UK2Node_CallFunction* AddItem = Call(G, 900, 2500, Inv, GET_FUNCTION_NAME_CHECKED(UMusubiThirdPersonInventory, AddItem));
	Default(Timer, TEXT("Time"), TEXT("2.0"));
	Default(Timer, TEXT("bLooping"), TEXT("true"));
	Default(AddGold, TEXT("Amount"), TEXT("10"));
	Default(AddItem, TEXT("Id"), TEXT("1"));
	Default(AddItem, TEXT("Count"), TEXT("1"));
	bOk &= Link(ABind->GetThenPin(), Timer->GetExecPin());
	bOk &= Link(Tick->FindPinChecked(UK2Node_Event::DelegateOutputName), Pin(Timer, TEXT("Delegate")));
	bOk &= Link(Tick->GetThenPin(), Branch2->GetExecPin());
	bOk &= Link(Owned->GetReturnValuePin(), Branch2->GetConditionPin());
	bOk &= Link(Branch2->GetThenPin(), AddGold->GetExecPin());
	bOk &= Link(AddGold->GetThenPin(), AddItem->GetExecPin());
	bOk &= Link(InvGet->GetValuePin(), Self(AddGold));
	bOk &= Link(InvGet->GetValuePin(), Self(AddItem));
	for (UEdGraphPin* P : OldThen) bOk &= Link(Timer->GetThenPin(), P);
	if (!bOk) return 1;

	FBlueprintEditorUtils::MarkBlueprintAsStructurallyModified(BP);
	FCompilerResultsLog Results;
	FKismetEditorUtilities::CompileBlueprint(BP, EBlueprintCompileOptions::None, &Results);
	if (Results.NumErrors > 0)
	{
		for (const TSharedRef<FTokenizedMessage>& M : Results.Messages) UE_LOG(LogMusubiDemo, Error, TEXT("compile: %s"), *M->ToText().ToString());
		return 1;
	}
	UPackage* Pkg = BP->GetOutermost();
	const FString File = FPackageName::LongPackageNameToFilename(Pkg->GetName(), FPackageName::GetAssetPackageExtension());
	FSavePackageArgs Args; Args.TopLevelFlags = RF_Public | RF_Standalone; Args.Error = GError;
	if (!UPackage::SavePackage(Pkg, BP, *File, Args)) { UE_LOG(LogMusubiDemo, Error, TEXT("save failed: %s"), *File); return 1; }
	UE_LOG(LogMusubiDemo, Display, TEXT("%s: %d node(s) replaced, graph compiled (%d warning(s)), saved %s"), *BP->GetName(), Removed, Results.NumWarnings, *File);
	return 0;
}
