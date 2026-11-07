
export interface GuestImageBox { readonly left: number; readonly top: number; readonly width: number; readonly height: number }
export interface GuestPoint { x: number; y: number }
export function mapGuestPoint(clientX: number, clientY: number, box: GuestImageBox, width: number, height: number): GuestPoint | null;