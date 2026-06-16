/// Semantic land-cover classification for each grid cell.
///
/// Precedence when categories overlap a cell (highest wins):
///   Water > BuildingWall > BuildingFloor = BuildingMid = BuildingTall > CliffFace > Stairs
///   > Road > Plaza = Sidewalk > Path > ParkGrass > Shoreline > Sand > Grass
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum SemanticClass {
    #[default]
    Grass         = 0,
    ParkGrass     = 1,
    Sand          = 2,
    Path          = 3,
    Sidewalk      = 4,
    Road          = 5,
    Stairs        = 6,
    CliffFace     = 7,
    BuildingFloor = 8,
    BuildingWall  = 9,
    Water         = 10,
    /// Open paved urban plaza — Civic Center, UN Plaza, Ferry Building forecourt.
    Plaza         = 11,
    /// Water/land transition band (auto-generated: land cells touching water).
    Shoreline     = 12,
    /// 4-7 story apartments, SoMa mid-rise.
    BuildingMid   = 13,
    /// 8+ story FiDi towers.
    BuildingTall  = 14,
}

impl SemanticClass {
    /// Higher value → higher precedence when cells overlap.
    pub fn precedence(self) -> u8 {
        match self {
            Self::Grass         => 0,
            Self::ParkGrass     => 1,
            Self::Shoreline     => 1,  // same as ParkGrass; set by post-pass, not write_if_higher
            Self::Sand          => 2,
            Self::Path          => 3,
            Self::Sidewalk      => 4,
            Self::Plaza         => 4,  // same as Sidewalk; Road(5) beats it
            Self::Road          => 5,
            Self::Stairs        => 6,
            Self::CliffFace     => 7,
            Self::BuildingFloor => 8,
            Self::BuildingMid   => 8,
            Self::BuildingTall  => 8,
            Self::BuildingWall  => 9,
            Self::Water         => 10,
        }
    }

    pub fn merge(self, other: Self) -> Self {
        if other.precedence() > self.precedence() { other } else { self }
    }

    /// Convert a raw u8 discriminant back to SemanticClass (unknown → Grass).
    pub fn from_u8(v: u8) -> Self {
        match v {
            1  => Self::ParkGrass,
            2  => Self::Sand,
            3  => Self::Path,
            4  => Self::Sidewalk,
            5  => Self::Road,
            6  => Self::Stairs,
            7  => Self::CliffFace,
            8  => Self::BuildingFloor,
            9  => Self::BuildingWall,
            10 => Self::Water,
            11 => Self::Plaza,
            12 => Self::Shoreline,
            13 => Self::BuildingMid,
            14 => Self::BuildingTall,
            _  => Self::Grass,
        }
    }
}

/// Autotile variant index (0–47 for blob/47-tile scheme, 0–15 for 4-bit edge).
pub type AutotileVariant = u8;

/// Composite render tile ID packed into u32:
///   bits 0..7   = SemanticClass as u8
///   bits 8..15  = AutotileVariant
///   bits 16..31 = reserved (LOD, atlas page, etc.)
pub fn make_tile_id(cls: SemanticClass, variant: AutotileVariant) -> u32 {
    (cls as u32) | ((variant as u32) << 8)
}

pub fn tile_id_class(id: u32) -> u8 {
    (id & 0xFF) as u8
}

pub fn tile_id_variant(id: u32) -> u8 {
    ((id >> 8) & 0xFF) as u8
}

/// Collision costs stored in the `collision` layer.
pub mod collision {
    pub const WALKABLE: u8      = 0;
    pub const GRASS_COST: u8    = 2;
    pub const PARK_COST: u8     = 1;
    pub const SIDEWALK_COST: u8 = 0;
    pub const ROAD_COST: u8     = 0;
    pub const PLAZA_COST: u8    = 0;
    pub const SHORELINE_COST: u8 = 1;
    pub const PATH_COST: u8     = 1;
    pub const STAIRS_COST: u8   = 8;
    pub const BLOCKED: u8       = 255;
}

/// A fixed-size 2-D grid stored row-major.
#[derive(Debug, Clone)]
pub struct Grid<T: Clone> {
    pub width: u32,
    pub height: u32,
    data: Vec<T>,
}

impl<T: Clone + Default> Grid<T> {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![T::default(); (width * height) as usize] }
    }
}

impl<T: Clone> Grid<T> {
    pub fn filled(width: u32, height: u32, value: T) -> Self {
        Self { width, height, data: vec![value; (width * height) as usize] }
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> &T {
        &self.data[(y * self.width + x) as usize]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, value: T) {
        self.data[(y * self.width + x) as usize] = value;
    }

    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    pub fn from_fn(width: u32, height: u32, mut f: impl FnMut(u32, u32) -> T) -> Self {
        let mut data = Vec::with_capacity((width * height) as usize);
        for row in 0..height {
            for col in 0..width {
                data.push(f(col, row));
            }
        }
        Self { width, height, data }
    }
}

/// Chunk coordinates in the integer chunk grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkCoord {
    pub cx: i32,
    pub cy: i32,
}

/// All outputs for one (chunk, lod) pair.
pub struct ChunkLod {
    pub coord: ChunkCoord,
    pub lod: u32,
    pub render: Grid<u32>,
    pub collision: Grid<u8>,
}
