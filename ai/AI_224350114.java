import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_224350114 extends AI {
public AI_224350114() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_f() throws LeekRunException {
ops(1);return 1l;
}
private Object f_g() throws LeekRunException {
ops(1);return 2l;
}
public Object runIA(Session session) throws LeekRunException {
ops(1); return (Object) add(f_f(), f_g());
}
protected String getAIString() { return "Main_1a99d16a5c859_24";}
protected String[] getErrorFiles() { return new String[] {"lib", "Main_1a99d16a5c859_24", };}

protected int[] getErrorFilesID() { return new int[] {108102, 224350114, };}

}
