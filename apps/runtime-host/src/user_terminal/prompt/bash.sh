# 保留进入提示符时的状态；PWD 的文本只能经变量展开进入 PS1，不能拼成可执行源码。
__ez_terminal_status=$?
    if [ -n "${EZ_ASSISTANT_TOOL_BIN-}" ]; then
        case ":$PATH:" in
            *":$EZ_ASSISTANT_TOOL_BIN:"*) ;;
            *) export PATH="$EZ_ASSISTANT_TOOL_BIN${PATH:+:$PATH}" ;;
        esac
    fi
__ez_terminal_prompt() {
    local previous=$1 text=$PS1 output= char next index=0 replacement='\W'
    case "$PWD" in
        /) __ez_terminal_dir=/ ;;
        "$HOME") __ez_terminal_dir='~' ;;
        *) __ez_terminal_dir=${PWD##*/} ;;
    esac
    __ez_terminal_dir=${__ez_terminal_dir//[[:cntrl:]]/?}
    if [ "${#__ez_terminal_dir}" -gt 32 ]; then
        __ez_terminal_dir=${__ez_terminal_dir:0:29}...
    fi
    if shopt -q promptvars; then replacement='${__ez_terminal_dir}'; fi
    while [ "$index" -lt "${#text}" ]; do
        char=${text:index:1}
        index=$((index + 1))
        if [ "$char" = '\' ] && [ "$index" -lt "${#text}" ]; then
            next=${text:index:1}
            index=$((index + 1))
            if [ "$next" = D ] && [ "${text:index:1}" = '{' ]; then
                output=$output$char$next
                while [ "$index" -lt "${#text}" ]; do
                    char=${text:index:1}
                    index=$((index + 1))
                    output=$output$char
                    [ "$char" = '}' ] && break
                done
                continue
            fi
            case "$next" in
                w|W) output=$output$replacement ;;
                *) output=$output$char$next ;;
            esac
        else
            output=$output$char
        fi
    done
    PS1=$output
    return "$previous"
}
__ez_terminal_prompt "$__ez_terminal_status"
