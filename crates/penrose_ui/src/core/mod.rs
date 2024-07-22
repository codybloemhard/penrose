//! The core [`Draw`] and [`Context`] structs for rendering UI elements.
//!
//! If you are only interested in adding functionality to the penrose [StatusBar][0] then you
//! do not need to worry about the use and implementation of `Draw` and `Context`: the abstractions
//! provided by the [Widget][1] trait should be sufficient for your needs. If however you wish
//! to build your own minimal text based UI from scratch then your might find these structs useful.
//!
//! > **NOTE**: As mentioned in the crate level docs, this crate is definitely not intended as a
//! > fully general purpose graphics API. You are unlikely to find full support for operations that
//! > are not required for implementing a simple text based status bar.
//!
//!   [0]: crate::StatusBar
//!   [1]: crate::bar::widgets::Widget
use crate::{Error, Result};
use penrose::{
    pure::geometry::{Point, Rect},
    x::{WinType, XConn},
    x11rb::RustConn,
    Color, Xid,
};
use std::{
    alloc::{alloc, dealloc, handle_alloc_error, Layout},
    cmp::max,
    collections::{hash_map::Entry, HashMap},
    ffi::CString,
};
use tracing::{debug, info};
use x11::{
    xft::{XftColor, XftColorAllocName, XftDraw, XftDrawCreate, XftDrawDestroy, XftDrawStringUtf8},
    xlib::{
        CapButt, Complex, CoordModeOrigin, Display, Drawable, False, JoinMiter, LineSolid, Window,
        XCopyArea, XCreateGC, XCreatePixmap, XDefaultColormap, XDefaultDepth, XDefaultVisual,
        XDrawRectangle, XFillPolygon, XFillRectangle, XFreeGC, XFreePixmap, XOpenDisplay, XPoint,
        XSetForeground, XSetGraphicsExposures, XSetLineAttributes, XSync, GC,
    },
};

mod fontset;
use fontset::Fontset;

// Xlib manual: https://www.x.org/releases/current/doc/libX11/libX11/libX11.pdf

pub(crate) const SCREEN: i32 = 0;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
/// A set of styling options for a text string that is to be rendered using [Draw].
///
/// The font itself is specified on the [Draw] instance when it is created or by using the
/// `set_font` method.
pub struct TextStyle {
    /// The foreground color to be used for rendering the text itself.
    pub fg: Color,
    /// The background color for the region behind the text (defaults to the [Draw] background if None).
    pub bg: Option<Color>,
    /// Padding in pixels around the text to the left and right.
    pub padding: (u32, u32),
}

#[derive(Debug)]
struct Surface {
    drawable: Drawable,
    gc: GC,
    r: Rect,
    id: u64,
}

impl Surface {
    /// SAFETY: dpy must be non-null
    pub(crate) unsafe fn flush(&self, dpy: *mut Display) {
        let Rect { w, h, .. } = self.r;
        XCopyArea(dpy, self.drawable, self.id, self.gc, 0, 0, w, h, 0, 0);
        XSync(dpy, False);
    }
}

/// A minimal back end for rendering simple text based UIs.
///
/// > **NOTE**: Your application should create a single [Draw] struct to manage the windows and
/// > surfaces it needs to render your UI. See the [Context] struct for how to draw to the surfaces
/// > you have created.
///
/// # Fonts
/// ### Specifying fonts
/// Font names need to be in a form that can be parsed by `xft`. The simplest way to find the valid
/// font names on your system is via the `fc-list` program like so:
/// ```sh
/// $ fc-list -f '%{family}\n' | sort -u
/// ```
/// [Draw] will automtically append `:size={point_size}` to the font name when loading the font via
/// xft. The [Arch wiki page on fonts][0] is a useful resource on how X11 fonts work if you are
/// interested in futher reading.
///
/// ### Font fallback for missing glyphs
/// [Draw] makes use of [fontconfig][1] to locate appropriate fallback fonts on your system when a
/// glyph is encountered that the primary font does not support. If you wish to modify how fallback
/// fonts are selected you will need to modify your [font-conf][2] (the Arch wiki has a [good page][3]
/// on how to do this if you are looking for a reference).
///
/// # Example usage
/// > Please see the crate [examples directory][4] for more examples.
/// ```no_run
/// use penrose::{
///     pure::geometry::Rect,
///     x::{Atom, WinType},
///     Color,
/// };
/// use penrose_ui::Draw;
/// use std::{thread::sleep, time::Duration};
///
/// let fg = Color::try_from("#EBDBB2").unwrap();
/// let bg = Color::try_from("#282828").unwrap();
/// let mut drw = Draw::new("mono", 12, bg).unwrap();
/// let w = drw.new_window(
///     WinType::InputOutput(Atom::NetWindowTypeDock),
///     Rect::new(0, 0, 300, 50),
///     false,
/// ).unwrap();
///
/// let mut ctx = drw.context_for(w).unwrap();
/// ctx.draw_text("Hello from penrose_ui!", 0, (10, 0), fg).unwrap();
/// ctx.flush();
/// drw.flush(w).unwrap();
///
/// sleep(Duration::from_secs(2));
/// ```
///
///  [0]: https://wiki.archlinux.org/title/Fonts
///  [1]: https://www.freedesktop.org/wiki/Software/fontconfig/
///  [2]: https://man.archlinux.org/man/fonts-conf.5
///  [3]: https://wiki.archlinux.org/title/Font_configuration#Set_default_or_fallback_fonts
///  [4]: https://github.com/sminez/penrose/tree/develop/crates/penrose_ui/examples
#[derive(Debug)]
pub struct Draw {
    pub(crate) conn: RustConn,
    dpy: *mut Display,
    fss: HashMap<String, Fontset>,
    bg: Color,
    surfaces: HashMap<Xid, Surface>,
    colors: HashMap<Color, XColor>,
    active_font: String,
}

impl Drop for Draw {
    fn drop(&mut self) {
        // SAFETY: all pointers being freed are known to be non-null
        unsafe {
            for (_, s) in self.surfaces.drain() {
                XFreePixmap(self.dpy, s.drawable);
                XFreeGC(self.dpy, s.gc);
            }
        }
    }
}

fn font_key(font: &str, point_size: u8) -> String {
    format!("{font}:size={point_size}")
}

impl Draw {
    /// Construct a new [Draw] instance using the specified font and background color.
    ///
    /// ### Font names
    /// See the top level docs for [Draw] for details on how fonts are specified.
    ///
    /// ### Errors
    /// This method will error if it is unable to establish a connection with the X server.
    pub fn new(font: &str, point_size: u8, bg: impl Into<Color>) -> Result<Self> {
        let conn = RustConn::new()?;
        // SAFETY:
        //   - passing NULL as the argument here is valid as documented here: https://man.archlinux.org/man/extra/libx11/XOpenDisplay.3.en
        let dpy = unsafe { XOpenDisplay(std::ptr::null()) };
        let mut colors = HashMap::new();
        let bg = bg.into();
        colors.insert(bg, XColor::try_new(dpy, &bg)?);

        let k = font_key(font, point_size);
        let fs = Fontset::try_new(dpy, &k)?;
        let mut fss = HashMap::new();
        fss.insert(k.clone(), fs);

        Ok(Self {
            conn,
            dpy,
            fss,
            surfaces: HashMap::new(),
            bg,
            colors,
            active_font: k,
        })
    }

    /// Get access to the underlying [XConn] used by this [Draw].
    pub fn conn(&self) -> &impl XConn {
        &self.conn
    }

    /// Create a new X window with an initialised surface for drawing.
    ///
    /// Destroying this window should be carried out using the `destroy_window_and_surface` method
    /// so that the associated graphics state is also cleaned up correctly.
    pub fn new_window(&mut self, ty: WinType, r: Rect, managed: bool) -> Result<Xid> {
        info!(?ty, ?r, %managed, "creating new window");
        let id = self.conn.create_window(ty, r, managed)?;

        debug!("initialising graphics context and pixmap");
        let root = *self.conn.root() as Window;
        // SAFETY: self.dpy is non-null and screen index 0 is always valid
        let (drawable, gc) = unsafe {
            let depth = XDefaultDepth(self.dpy, SCREEN) as u32;
            let drawable = XCreatePixmap(self.dpy, root, r.w, r.h, depth);
            let gc = XCreateGC(self.dpy, root, 0, std::ptr::null_mut());
            XSetLineAttributes(self.dpy, gc, 1, LineSolid, CapButt, JoinMiter);
            XSetGraphicsExposures(self.dpy, gc, False);

            (drawable, gc)
        };

        self.surfaces.insert(
            id,
            Surface {
                id: *id as u64,
                r,
                gc,
                drawable,
            },
        );

        Ok(id)
    }

    /// Destroy the specified window along with any surface and graphics context state held
    /// within this draw.
    pub fn destroy_window_and_surface(&mut self, id: Xid) -> Result<()> {
        if let Some(s) = self.surfaces.remove(&id) {
            self.conn.destroy_window(id)?;
            // SAFETY: the pointers being freed are known to be non-null
            unsafe {
                XFreePixmap(self.dpy, s.drawable);
                XFreeGC(self.dpy, s.gc);
            }
        }

        Ok(())
    }

    pub(crate) fn add_font(&mut self, font: &str, point_size: u8) -> Result<()> {
        let k = font_key(font, point_size);
        if let Entry::Vacant(e) = self.fss.entry(k) {
            let fs = Fontset::try_new(self.dpy, e.key())?;
            e.insert(fs);
        }

        Ok(())
    }

    /// Set the font being used for rendering text and clear the existing cache of fallback fonts
    /// for characters that are not supported by the primary font.
    pub fn set_font(&mut self, font: &str, point_size: u8) -> Result<()> {
        self.add_font(font, point_size)?;
        self.active_font = font_key(font, point_size);

        Ok(())
    }

    /// Retrieve the drawing [Context] for the given window `Xid`.
    ///
    /// This method will error if the requested id does not already have an initialised surface.
    /// See the `new_window` method for details.
    pub fn context_for(&mut self, id: Xid) -> Result<Context<'_>> {
        let s = self
            .surfaces
            .get(&id)
            .ok_or(Error::UnintialisedSurface { id })?;

        Ok(Context {
            dx: 0,
            dy: 0,
            dpy: self.dpy,
            s,
            bg: self.bg,
            fs: self
                .fss
                .get_mut(&self.active_font)
                .expect("active_font to be present"),
            colors: &mut self.colors,
        })
    }

    /// Flush any pending requests to the X server and map the specifed window to the screen.
    pub fn flush(&self, id: Xid) -> Result<()> {
        if let Some(s) = self.surfaces.get(&id) {
            // SAFETY: self.dpy is non-null
            unsafe { s.flush(self.dpy) };
            self.conn.map(id)?;
            self.conn.flush();
        };

        Ok(())
    }
}

/// A minimal drawing context for rendering text based UI elements
///
/// A [Context] provides you with a backing pixmap for rendering your UI using simple offset and
/// rendering operations. By default, the context will be positioned in the top left corner of
/// the parent window created by your [Draw]. You can use the `translate` and `set/reset` offset
/// methods to modify where the next drawing operation will take place.
///
/// > It is worthwhile looking at the implementation of the [StatusBar][0] struct and how it
/// > handles rendering child widgets for a real example of how to make use of the offseting
/// > functionality of this struct.
///
///   [0]: crate::StatusBar
#[derive(Debug)]
pub struct Context<'a> {
    dx: i32,
    dy: i32,
    dpy: *mut Display,
    s: &'a Surface,
    bg: Color,
    fs: &'a mut Fontset,
    colors: &'a mut HashMap<Color, XColor>,
}

impl<'a> Context<'a> {
    /// Clear the underlying surface, restoring it to the background color.
    pub fn clear(&mut self) -> Result<()> {
        self.fill_rect(Rect::new(0, 0, self.s.r.w, self.s.r.h), self.bg)
    }

    /// Offset future drawing operations by an additional (dx, dy)
    pub fn translate(&mut self, dx: i32, dy: i32) {
        self.dx += dx;
        self.dy += dy;
    }

    /// Set future drawing operations to apply from a specified point.
    pub fn set_offset(&mut self, x: i32, y: i32) {
        self.dx = x;
        self.dy = y;
    }

    /// Set an absolute x offset for future drawing operations.
    pub fn set_x_offset(&mut self, x: i32) {
        self.dx = x;
    }

    /// Set an absolute y offset for future drawing operations.
    pub fn set_y_offset(&mut self, y: i32) {
        self.dy = y;
    }

    /// Set future drawing operations to apply from the origin.
    pub fn reset_offset(&mut self) {
        self.dx = 0;
        self.dy = 0;
    }

    fn get_or_try_init_xcolor(&mut self, c: Color) -> Result<*mut XftColor> {
        if let Some(xc) = self.colors.get(&c) {
            return Ok(xc.0);
        }

        let xc = XColor::try_new(self.dpy, &c)?;
        let ptr = xc.0;
        self.colors.insert(c, xc);

        Ok(ptr)
    }

    /// Render a rectangular border using the supplied color.
    pub fn draw_rect(&mut self, Rect { x, y, w, h }: Rect, color: Color) -> Result<()> {
        let xcol = self.get_or_try_init_xcolor(color)?;
        let (x, y) = (self.dx + x as i32, self.dy + y as i32);

        // SAFETY:
        //   - the pointers for self.dpy, s.drawable, s.gc are known to be non-null
        //   - xcol is known to be non-null so dereferencing is safe
        unsafe {
            XSetForeground(self.dpy, self.s.gc, (*xcol).pixel);
            XDrawRectangle(self.dpy, self.s.drawable, self.s.gc, x, y, w, h);
        }

        Ok(())
    }

    /// Render a filled rectangle using the supplied color.
    pub fn fill_rect(&mut self, Rect { x, y, w, h }: Rect, color: Color) -> Result<()> {
        let xcol = self.get_or_try_init_xcolor(color)?;
        let (x, y) = (self.dx + x as i32, self.dy + y as i32);

        // SAFETY:
        //   - the pointers for self.dpy, s.drawable, s.gc are known to be non-null
        //   - xcol is known to be non-null so dereferencing is safe
        unsafe {
            XSetForeground(self.dpy, self.s.gc, (*xcol).pixel);
            XFillRectangle(self.dpy, self.s.drawable, self.s.gc, x, y, w, h);
        }

        Ok(())
    }

    /// Render a filled rectangle using the supplied color.
    pub fn fill_polygon(&mut self, points: &[Point], color: Color) -> Result<()> {
        let xcol = self.get_or_try_init_xcolor(color)?;
        let mut xpoints: Vec<XPoint> = points
            .iter()
            .map(|&Point { x, y }| XPoint {
                x: self.dx as i16 + x as i16,
                y: self.dy as i16 + y as i16,
            })
            .collect();

        // SAFETY:
        //   - the pointers for self.dpy, s.drawable, s.gc are known to be non-null
        //   - xcol is known to be non-null so dereferencing is safe
        unsafe {
            XSetForeground(self.dpy, self.s.gc, (*xcol).pixel);
            XFillPolygon(
                self.dpy,
                self.s.drawable,
                self.s.gc,
                &mut xpoints[0] as *mut _,
                points.len() as i32,
                Complex, // we could be smarter here but for now this works
                CoordModeOrigin,
            );
        }

        Ok(())
    }

    /// Fill the specified area with this Context's background color
    pub fn fill_bg(&mut self, r: Rect) -> Result<()> {
        self.fill_rect(r, self.bg)
    }

    /// Render the provided text at the current context offset using the supplied color.
    pub fn draw_text(
        &mut self,
        txt: &str,
        h_offset: u32,
        padding: (u32, u32),
        c: Color,
    ) -> Result<(u32, u32)> {
        // SAFETY:
        //   - the pointers for self.dpy and s.drawable are known to be non-null
        //   - we wrap the returned pointer in DropXftDraw to ensure that we correctly destroy
        //     the XftDraw we create here (see below)
        let d = unsafe {
            XftDrawCreate(
                self.dpy,
                self.s.drawable,
                XDefaultVisual(self.dpy, SCREEN),
                XDefaultColormap(self.dpy, SCREEN),
            )
        };

        let _drop_draw = DropXftDraw { ptr: d };

        let (lpad, rpad) = (padding.0 as i32, padding.1);
        let (mut x, y) = (lpad + self.dx, self.dy);
        let (mut total_w, mut total_h) = (x as u32, 0);
        let xcol = self.get_or_try_init_xcolor(c)?;

        for (chunk, fm) in self.fs.per_font_chunks(txt).into_iter() {
            let fnt = self.fs.fnt(fm);
            let (chunk_w, chunk_h) = fnt.get_exts(self.dpy, chunk)?;

            // SAFETY: fnt pointer is non-null
            let chunk_y = unsafe { y + h_offset as i32 + (*fnt.xfont).ascent };
            let c_str = CString::new(chunk)?;

            // SAFETY:
            // - fnt.xfont is known to be non-null
            // - the string character pointer and length have been obtained from a Rust CString
            unsafe {
                XftDrawStringUtf8(
                    d,
                    xcol,
                    fnt.xfont,
                    x,
                    chunk_y,
                    c_str.as_ptr() as *mut _,
                    c_str.as_bytes().len() as i32,
                );
            }

            x += chunk_w as i32;
            total_w += chunk_w;
            total_h = max(total_h, chunk_h);
        }

        return Ok((total_w + rpad, total_h));

        // There are multiple error paths here where we need to make sure that we correctly destroy
        // the XftDraw we created. Rather than complicate the error handling we use a Drop wrapper
        // to ensure that we run XftDrawDestroy when the function returns.

        struct DropXftDraw {
            ptr: *mut XftDraw,
        }

        impl Drop for DropXftDraw {
            fn drop(&mut self) {
                // SAFETY: the pointer we have must be non-null
                unsafe { XftDrawDestroy(self.ptr) };
            }
        }
    }

    /// Determine the width and height taken up by a given string in pixels.
    pub fn text_extent(&mut self, txt: &str) -> Result<(u32, u32)> {
        let (mut w, mut h) = (0, 0);
        for (chunk, fm) in self.fs.per_font_chunks(txt) {
            let (cw, ch) = self.fs.fnt(fm).get_exts(self.dpy, chunk)?;
            w += cw;
            h = max(h, ch);
        }

        Ok((w, h))
    }

    /// Flush pending requests to the X server.
    ///
    /// This method does not need to be called explicitly if the flush method for
    /// the parent [Draw] is being called as well.
    pub fn flush(&self) {
        // SAFETY: self.dpy is non-null
        unsafe { self.s.flush(self.dpy) }
    }
}

#[derive(Debug)]
struct XColor(*mut XftColor);

impl Drop for XColor {
    fn drop(&mut self) {
        let layout = Layout::new::<XftColor>();
        // SAFETY: the memory being deallocated was allocated on creation of the XColor
        unsafe { dealloc(self.0 as *mut u8, layout) }
    }
}

impl XColor {
    fn try_new(dpy: *mut Display, c: &Color) -> Result<Self> {
        // SAFETY: this private method is only called with a non-null dpy pointer
        let inner = unsafe { try_xftcolor_from_name(dpy, &c.as_rgb_hex_string())? };

        Ok(Self(inner))
    }
}

unsafe fn try_xftcolor_from_name(dpy: *mut Display, color: &str) -> Result<*mut XftColor> {
    // https://doc.rust-lang.org/std/alloc/trait.GlobalAlloc.html#tymethod.alloc
    let layout = Layout::new::<XftColor>();
    let ptr = alloc(layout);
    if ptr.is_null() {
        handle_alloc_error(layout);
    }

    let c_name = CString::new(color)?;
    let res = XftColorAllocName(
        dpy,
        XDefaultVisual(dpy, SCREEN),
        XDefaultColormap(dpy, SCREEN),
        c_name.as_ptr(),
        ptr as *mut XftColor,
    );

    if res == 0 {
        Err(Error::UnableToAllocateColor)
    } else {
        Ok(ptr as *mut XftColor)
    }
}
