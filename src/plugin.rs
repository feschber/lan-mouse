use input_capture::CaptureEvent;

struct PluginManager {
    capture_hook: Vec<Box<dyn Fn(CaptureEvent)>>,
    capture_transform: Vec<Box<dyn Fn(CaptureEvent) -> CaptureEvent>>,
}
