using System.Runtime.InteropServices;

namespace Ivy.PhoneBridge;

// Windows SDK10.0.26100.0 UIAutomationClient.h, IUIAutomation2's complete inherited slot order.
// Unused members deliberately have no callable native signature. Never invoke a Reserved member.
public static partial class WindowsDesktopControls {
    [ComImport, Guid("34723aff-0c9d-49d0-9896-7ab52df8cd8a"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IAutomation {
        void Reserved_CompareElements();
        void Reserved_CompareRuntimeIds();
        void Reserved_GetRootElement();
        [PreserveSig] int ElementFromHandle(IntPtr window, out IElement element);
        void Reserved_ElementFromPoint();
        void Reserved_GetFocusedElement();
        void Reserved_GetRootElementBuildCache();
        void Reserved_ElementFromHandleBuildCache();
        void Reserved_ElementFromPointBuildCache();
        void Reserved_GetFocusedElementBuildCache();
        [PreserveSig] int CreateTreeWalker([MarshalAs(UnmanagedType.IUnknown)] object condition, out IWalker walker);
        void Reserved_get_ControlViewWalker();
        void Reserved_get_ContentViewWalker();
        [PreserveSig] int RawWalker(out IWalker walker);
        void Reserved_get_RawViewCondition();
        void Reserved_get_ControlViewCondition();
        void Reserved_get_ContentViewCondition();
        [PreserveSig] int CreateCacheRequest(out ICache cache);
        void Reserved_CreateTrueCondition();
        void Reserved_CreateFalseCondition();
        [PreserveSig] int CreatePropertyCondition(int property, [MarshalAs(UnmanagedType.Struct)] object value, [MarshalAs(UnmanagedType.IUnknown)] out object condition);
        void Reserved_CreatePropertyConditionEx();
        [PreserveSig] int CreateAndCondition([MarshalAs(UnmanagedType.IUnknown)] object first, [MarshalAs(UnmanagedType.IUnknown)] object second, [MarshalAs(UnmanagedType.IUnknown)] out object condition);
        void Reserved_CreateAndConditionFromArray();
        void Reserved_CreateAndConditionFromNativeArray();
        [PreserveSig] int CreateOrCondition([MarshalAs(UnmanagedType.IUnknown)] object first, [MarshalAs(UnmanagedType.IUnknown)] object second, [MarshalAs(UnmanagedType.IUnknown)] out object condition);
        void Reserved_CreateOrConditionFromArray();
        void Reserved_CreateOrConditionFromNativeArray();
        void Reserved_CreateNotCondition();
        void Reserved_AddAutomationEventHandler();
        void Reserved_RemoveAutomationEventHandler();
        void Reserved_AddPropertyChangedEventHandlerNativeArray();
        void Reserved_AddPropertyChangedEventHandler();
        void Reserved_RemovePropertyChangedEventHandler();
        void Reserved_AddStructureChangedEventHandler();
        void Reserved_RemoveStructureChangedEventHandler();
        void Reserved_AddFocusChangedEventHandler();
        void Reserved_RemoveFocusChangedEventHandler();
        void Reserved_RemoveAllEventHandlers();
        void Reserved_IntNativeArrayToSafeArray();
        void Reserved_IntSafeArrayToNativeArray();
        void Reserved_RectToVariant();
        void Reserved_VariantToRect();
        void Reserved_SafeArrayToRectNativeArray();
        void Reserved_CreateProxyFactoryEntry();
        void Reserved_get_ProxyFactoryMapping();
        void Reserved_GetPropertyProgrammaticName();
        void Reserved_GetPatternProgrammaticName();
        void Reserved_PollForPotentialSupportedPatterns();
        void Reserved_PollForPotentialSupportedProperties();
        void Reserved_CheckNotSupported();
        void Reserved_get_ReservedNotSupportedValue();
        void Reserved_get_ReservedMixedAttributeValue();
        void Reserved_ElementFromIAccessible();
        void Reserved_ElementFromIAccessibleBuildCache();
        void Reserved_get_AutoSetFocus();
        [PreserveSig] int SetAutoFocus([MarshalAs(UnmanagedType.Bool)] bool enabled);
        void Reserved_get_ConnectionTimeout();
        [PreserveSig] int SetConnectionTimeout(uint milliseconds);
        void Reserved_get_TransactionTimeout();
        [PreserveSig] int SetTransactionTimeout(uint milliseconds);
    }
    [ComImport, Guid("d22108aa-8ac5-49a5-837b-37bbb3d7591e"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IElement {
        void ReservedSetFocus(); void ReservedRuntimeId(); void ReservedFindFirst(); void ReservedFindAll(); void ReservedFindFirstCached();
        [PreserveSig] int FindAllCached(int scope, [MarshalAs(UnmanagedType.IUnknown)] object condition, ICache cache, out IElements elements);
        [PreserveSig] int UpdateCache(ICache cache, out IElement element);
        [PreserveSig] int Current(int property, [MarshalAs(UnmanagedType.Struct)] out object value);
        [PreserveSig] int CurrentEx(int property, [MarshalAs(UnmanagedType.Bool)] bool ignoreDefault, [MarshalAs(UnmanagedType.Struct)] out object value);
        [PreserveSig] int Cached(int property, [MarshalAs(UnmanagedType.Struct)] out object value);
        [PreserveSig] int CachedEx(int property, [MarshalAs(UnmanagedType.Bool)] bool ignoreDefault, [MarshalAs(UnmanagedType.Struct)] out object value);
        [PreserveSig] int GetCurrentPatternAs(int patternId, ref Guid riid, out IInvokePattern pattern);
    }
    [ComImport, Guid("fb377fbe-8ea6-46d5-9c73-6499642d3059"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IInvokePattern { [PreserveSig] int Invoke(); }
    [ComImport, Guid("14314595-b4bc-4055-95f2-58f2e42c9855"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IElements {
        [PreserveSig] int Length(out int length);
        [PreserveSig] int At(int index, out IElement element);
    }
    [ComImport, Guid("b32a92b5-bc25-4078-9c08-d7ee95c48e03"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface ICache { [PreserveSig] int AddProperty(int property); }
    [ComImport, Guid("4042c624-389c-4afc-a630-9df854a541fc"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IWalker {
        [PreserveSig] int Parent(IElement element, out IElement parent);
        void ReservedFirstChild(); void ReservedLastChild(); void ReservedNextSibling(); void ReservedPreviousSibling();
        [PreserveSig] int Normalize(IElement element, out IElement ancestor);
        [PreserveSig] int ParentCached(IElement element, ICache cache, out IElement parent);
    }
}
