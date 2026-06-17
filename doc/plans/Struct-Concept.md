# Struct and Typedef concept

The idea is that structures are kind of an in-betweener of a class and a struct. In the language itself, these things should be called a "type", while a C style typedef should be known as an "alias". Those structs itself should be known as a "type-struct" internally by documentation.

## Aliases

An alias should be easy to define by using this syntax:

```
alias uint32_t u32
```

Of course, this can also be used to alias to type-structs later.

## Type-Structs

These are a bit more complicated by syntax:

```
type SizedString {
  hidden u8 data
  hidden u64 size

  # A function that is "static" and does not access internal fields is detected by a lack of a "this"
  fn from_cstring(u8 *str) -> SizedString {
    u64 len = strlen(str)
    u8 *data = malloc(len-1)
    memcpy(data, str, len);
    # The initializer list works here because we're in the context of the type. It wouldn't work elsewhere because we cannot name the fields, as they are hidden.
    return SizedString {
      data = data,
      size = len
    }
  }

  # Functions marked const should guarantee that variables are never changed inside
  const fn to_cstring(this) -> u8* {
    # For this objects, the underscore is removed because 
    u8 *str = malloc(this.size + 1)
    str[this.size] = 0
    memcpy(str, this.data, this.size)
    return str
  }

  const fn get_data(this) -> u8* {
    return this.data
  }

  const fn get_size(this) -> u64 {
    return this.size
  }

  fn append(this, SizedString ss) {
    u64 sz = ss.get_size()
    this.data = realloc(this.data, this.size + sz)
    memcpy(&this.data[this.size], ss.get_data(), sz)
  }
}

# And outside we'd then use it as such. 

fn main(u32 argc, u8** argv) -> i32 {
  u8* msg = "Hello, World!"
  SizedString string = SizedString.from_cstring(msg);
  # ... do other stuff
}
```
